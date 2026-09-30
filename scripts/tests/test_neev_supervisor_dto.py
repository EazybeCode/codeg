"""Strict DTO and channel tests for the Neev supervisor control protocol."""

from dataclasses import FrozenInstanceError, replace
import importlib.util
import json
from pathlib import Path
import sys
import traceback
import unittest


OPERATIONS = Path(__file__).parents[1] / "operations"
MODULE_PATH = OPERATIONS / "neev_supervisor_dto.py"
WIRE_PATH = OPERATIONS / "neev_supervisor_wire.py"
FIXTURE_PATH = Path(__file__).parent / "fixtures" / "neev-control-dto-v1.json"
I64_MAX = 9223372036854775807


def load_module(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    if spec is None or spec.loader is None:
        raise RuntimeError("unable to load test module")
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


class BytesSubclass(bytes):
    pass


class SupervisorDtoTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.fixture = json.loads(FIXTURE_PATH.read_text(encoding="utf-8"))
        if (
            cls.fixture["format"] != "neev-control-dto-v1"
            or cls.fixture["synthetic"] is not True
        ):
            raise AssertionError("invalid DTO fixture metadata")
        cls.requests = {
            item["name"]: bytes.fromhex(item["payload_hex"])
            for item in cls.fixture["requests"]
        }
        cls.responses = {
            item["name"]: bytes.fromhex(item["payload_hex"])
            for item in cls.fixture["responses"]
        }
        cls.first_request_bytes = cls.requests["reconcile_first"]
        cls.first_response_bytes = cls.responses["reconcile_first_page"]

    def setUp(self):
        self.dto = load_module("neev_supervisor_dto", MODULE_PATH)

    def assert_dto_error(self, category, call, hidden=None):
        with self.assertRaises(self.dto.DtoError) as caught:
            call()
        self.assertEqual(caught.exception.category, category)
        self.assertEqual(str(caught.exception), category)
        self.assertEqual(repr(caught.exception), "DtoError(%r)" % category)
        if hidden is not None:
            self.assertNotIn(hidden, str(caught.exception))
            self.assertNotIn(hidden, repr(caught.exception))

    def request(self, name):
        return self.dto.decode_request(self.requests[name])

    def response(self, name):
        return self.dto.decode_response(self.responses[name])

    def reconcile_entry(
        self, entry_number, phase, handle=False, receipt=False, cleanup=False
    ):
        binding = self.request("check").body.ticket.binding
        return self.dto.ReconcileEntry(
            self.dto.Id(format(entry_number, "032x")),
            binding,
            self.dto.Id("5" * 32),
            self.dto.Id("6" * 32),
            phase,
            self.dto.Id("9" * 32) if handle else None,
            self.dto.Id("8" * 32) if receipt else None,
            self.dto.Id("7" * 32) if cleanup else None,
        )

    def test_first_page_roundtrip(self):
        request = self.request("reconcile_first")
        self.assertEqual(self.dto.decode_request(self.dto.encode_request(request)), request)
        response = self.response("reconcile_first_page")
        self.dto.validate_response(request, response, self.dto.Id("3" * 32))

    def test_fixture_covers_all_requests_and_response_variants(self):
        self.assertEqual(
            {item["expected_op"] for item in self.fixture["requests"]},
            {
                "start_login",
                "observe_login",
                "settle_login",
                "check_connection",
                "cleanup_connection",
                "reconcile_page",
                "submit_code",
            },
        )
        required_names = {
            "start_accepted",
            "start_replayed",
            "observe_queued",
            "observe_starting",
            "observe_awaiting_user",
            "observe_verifying",
            "observe_prepared",
            "observe_prepared_null",
            "observe_committed",
            "observe_failed_provider_unavailable",
            "observe_failed_reauth_required",
            "observe_failed_unsupported_account",
            "observe_expired",
            "observe_canceled",
            "observe_interrupted",
            "settle_committed",
            "check_queued",
            "check_checking",
            "check_finished_supported",
            "check_finished_disconnected",
            "check_finished_unsupported",
            "check_unavailable_provider_unavailable",
            "check_unavailable_interrupted",
            "check_unavailable_expired",
            "cleanup_pending_queued",
            "cleanup_pending_canceling",
            "cleanup_pending_logging_out",
            "cleanup_pending_stopping",
            "cleanup_pending_disposing",
            "cleanup_completed_confirmed",
            "cleanup_completed_not_confirmed",
            "cleanup_completed_not_needed",
            "cleanup_failed",
            "reconcile_first_page",
        }
        self.assertTrue(required_names.issubset(self.responses))
        for item in self.fixture["requests"]:
            with self.subTest(request=item["name"]):
                value = self.dto.decode_request(bytes.fromhex(item["payload_hex"]))
                self.assertEqual(value.op().value, item["expected_op"])
                self.assertEqual(
                    self.dto.decode_request(self.dto.encode_request(value)), value
                )
        requests = {item["name"]: self.request(item["name"]) for item in self.fixture["requests"]}
        for item in self.fixture["responses"]:
            with self.subTest(response=item["name"]):
                value = self.dto.decode_response(bytes.fromhex(item["payload_hex"]))
                self.assertEqual(value.outcome.value, item["expected_outcome"])
                self.assertEqual(value.tag(), item["expected_tag"])
                self.assertEqual(
                    self.dto.decode_response(self.dto.encode_response(value)), value
                )
                self.dto.validate_response(
                    requests[item["request_name"]], value, self.dto.Id("3" * 32)
                )

    def test_submit_code_op_and_provider_tagged_challenges(self):
        # The seventh op: submit_code carries a login binding + an authorization code
        # and answers with a login snapshot (same state machine as observe_login).
        submit = self.request("submit_code")
        self.assertIs(submit.op(), self.dto.Op.SUBMIT_CODE)
        self.assertEqual(submit.body.code.value, "abcd-1234-ABCD-EFGH")
        self.assertEqual(self.dto.decode_request(self.dto.encode_request(submit)), submit)
        answer = self.response("submit_verifying")
        self.assertIs(answer.op(), self.dto.Op.SUBMIT_CODE)
        self.assertEqual(answer.tag(), "verifying")
        self.dto.validate_response(submit, answer, self.dto.Id("3" * 32))

        # Codex device challenge vs Claude authorization-code challenge.
        device = self.response("observe_awaiting_user").body.challenge
        self.assertIsInstance(device, self.dto.DeviceChallenge)
        self.assertEqual(device.user_code.value, "ABCD-EFGH")
        claude = self.response("observe_awaiting_user_claude").body.challenge
        self.assertIsInstance(claude, self.dto.AuthorizationCodeChallenge)
        self.assertTrue(claude.verification_url.startswith("https://claude.com/cai/oauth/authorize"))

        # Each URL is pinned to its provider's origin.
        self.assert_dto_error(
            "invalid",
            lambda: self.dto.DeviceChallenge(
                "https://claude.com/cai/oauth/authorize",
                self.dto.UserCode("A-9"),
                self.dto.UnixSec(1),
            ),
        )
        self.assert_dto_error(
            "invalid",
            lambda: self.dto.AuthorizationCodeChallenge(
                "https://auth.openai.com/codex/device", self.dto.UnixSec(1)
            ),
        )
        # Gemini's NO_BROWSER sign-in is a paste-code flow at Google's authorize URL.
        google = self.dto.AuthorizationCodeChallenge(
            "https://accounts.google.com/o/oauth2/v2/auth?redirect_uri=x&state=y",
            self.dto.UnixSec(1),
        )
        self.assertIsInstance(google, self.dto.AuthorizationCodeChallenge)
        # Cursor's browser-poll challenge: cursor.com origin only, round-trips.
        browser = self.dto.BrowserChallenge(
            "https://cursor.com/loginDeepControl?challenge=x", self.dto.UnixSec(9)
        )
        encoded = self.dto._encode_login_challenge(browser)
        self.assertEqual(encoded["kind"], "browser")
        self.assertEqual(self.dto._login_challenge(encoded), browser)
        for bad_url in (
            "https://accounts.google.com.evil.example/o/oauth2/v2/auth?x",
            "https://cursor.com.evil.example/login",
            "http://cursor.com/login",
            "https://cursor.com/" + "x" * 1100,
        ):
            self.assert_dto_error(
                "invalid",
                lambda bad_url=bad_url: self.dto.BrowserChallenge(bad_url, self.dto.UnixSec(1)),
            )
        self.assert_dto_error(
            "invalid",
            lambda: self.dto.AuthorizationCodeChallenge(
                "https://cursor.com/loginDeepControl", self.dto.UnixSec(1)
            ),
        )
        self.assert_dto_error(
            "invalid",
            lambda: self.dto._login_challenge(
                {"kind": "browser", "verification_url": "https://cursor.com/x",
                 "expires_at": 1, "user_code": "A"}
            ),
        )
        # The authorization code is a secret: bounded, no whitespace/control, and
        # never reflected in repr/str.
        for bad in ("", "x" * 513, "has space", "tab\tx", "ctrl\x01x"):
            self.assert_dto_error("invalid", lambda bad=bad: self.dto.AuthorizationCode(bad))
        code = self.dto.AuthorizationCode("SECRET-CODE-1234")
        self.assertNotIn("SECRET-CODE-1234", repr(code) + str(code))
        self.assertNotIn("abcd-1234-ABCD-EFGH", repr(submit) + str(submit))

    def test_scalar_exact_boundaries_and_types(self):
        valid = (
            self.dto.Id("a" * 32),
            self.dto.SessionId("123e4567-e89b-42d3-a456-426614174000"),
            self.dto.Counter("1"),
            self.dto.Counter(str(I64_MAX)),
            self.dto.OwnerId(1),
            self.dto.OwnerId(2147483647),
            self.dto.UnixSec(0),
            self.dto.UnixSec(253402300799),
            self.dto.RetryMs(250),
            self.dto.RetryMs(30000),
            self.dto.Label("A" * 160),
            self.dto.Label("é" * 160),
            self.dto.UserCode("A-9"),
        )
        self.assertEqual(len(valid), 13)
        invalid = (
            (self.dto.Id, "A" * 32),
            (self.dto.Id, "a" * 31),
            (self.dto.SessionId, "123e4567-e89b-52d3-a456-426614174000"),
            (self.dto.SessionId, "123e4567-e89b-42d3-7456-426614174000"),
            (self.dto.Counter, "0"),
            (self.dto.Counter, "01"),
            (self.dto.Counter, str(I64_MAX + 1)),
            (self.dto.OwnerId, True),
            (self.dto.OwnerId, 0),
            (self.dto.OwnerId, 2147483648),
            (self.dto.UnixSec, -1),
            (self.dto.UnixSec, 253402300800),
            (self.dto.RetryMs, 249),
            (self.dto.RetryMs, 30001),
            (self.dto.Label, ""),
            (self.dto.Label, "x" * 161),
            (self.dto.Label, "a\u202eb"),
            (self.dto.Label, "\x7f"),
            (self.dto.UserCode, "lower"),
            (self.dto.UserCode, "A" * 65),
        )
        for constructor, value in invalid:
            with self.subTest(constructor=constructor.__name__, value_type=type(value)):
                self.assert_dto_error("invalid", lambda c=constructor, v=value: c(v))

    def test_fixed_wire_tags_are_retained_as_typed_fields(self):
        self.assertIs(
            self.request("settle").body.witness.kind,
            self.dto.WitnessKind.COMPLETED_CONNECT,
        )
        self.assertIs(
            self.response("start_accepted").body.disposition,
            self.dto.Disposition.ACCEPTED,
        )
        self.assertIs(
            self.response("settle_committed").body.state,
            self.dto.CommittedState.COMMITTED,
        )
        self.assertIs(
            self.response("cleanup_failed").body.reason,
            self.dto.CleanupFailedReason.CLEANUP_FAILED,
        )

    def test_closed_tags_and_nested_values_are_revalidated_at_boundaries(self):
        class EqualityCompatibleTag:
            value = "SYNTHETIC-UNKNOWN-STATE"

            def __eq__(self, _other):
                return True

        lookalike = EqualityCompatibleTag()
        for constructor, arguments in (
            (
                self.dto.LoginProgress,
                (lookalike, self.dto.UnixSec(1), self.dto.RetryMs(250)),
            ),
            (self.dto.LoginTerminal, (lookalike,)),
            (
                self.dto.CheckPending,
                (lookalike, self.dto.Id("a" * 32), self.dto.RetryMs(250)),
            ),
        ):
            with self.subTest(constructor=constructor.__name__):
                self.assert_dto_error("invalid", lambda c=constructor, a=arguments: c(*a))

        observe_request = self.request("observe")
        invalid_response = self.response("observe_queued")
        object.__setattr__(invalid_response.body, "state", lookalike)
        self.assert_dto_error("invalid", lambda: self.dto.encode_response(invalid_response))
        self.assert_dto_error(
            "invalid",
            lambda: self.dto.validate_response(
                observe_request, invalid_response, self.dto.Id("3" * 32)
            ),
        )
        client = self.dto.ClientChannel(
            observe_request.coordinator, self.dto.Id("3" * 32)
        )
        client.begin(observe_request)
        self.assert_dto_error("invalid", lambda: client.finish(invalid_response))
        self.assert_dto_error("state", lambda: client.begin(observe_request))

        invalid_request = self.request("cleanup")
        object.__setattr__(
            invalid_request.body.ticket, "result_generation", self.dto.Counter("3")
        )
        self.assert_dto_error("invalid", lambda: self.dto.encode_request(invalid_request))
        server = self.dto.ServerChannel(
            invalid_request.coordinator, self.dto.Id("3" * 32)
        )
        self.assert_dto_error("invalid", lambda: server.accept(invalid_request))
        self.assert_dto_error("state", lambda: server.accept(invalid_request))

    def test_strict_syntax_duplicate_numeric_and_extent_boundaries(self):
        raw = self.first_request_bytes
        mutations = (
            raw.replace(b'"limit":1', b'"limit":1,"\\u006cimit":1'),
            b"\xef\xbb\xbf" + raw,
            raw + b" x",
            raw.replace(b'"limit":1', b'"limit":true'),
            raw.replace(b'"limit":1', b'"limit":1.0'),
            raw.replace(b'"limit":1', b'"limit":1e0'),
            raw.replace(b'"limit":1', b'"limit":-0'),
            raw.replace(b'"limit":1', b'"limit":1,"extra":null'),
            raw.replace(b'"limit":1', b'"limit":"1"'),
            raw.replace(b'"boot_id":"1', b'"boot_id":"A'),
            b'{"v":1,"x":"\\ud800"}',
            b"[[[[[[[[[[[[[[[[[0]]]]]]]]]]]]]]]]]",
        )
        for value in mutations:
            with self.subTest(raw=value[:40]):
                self.assert_dto_error("invalid", lambda value=value: self.dto.decode_request(value))
        self.assert_dto_error("invalid", lambda: self.dto.decode_request(bytearray(raw)))
        self.assert_dto_error("invalid", lambda: self.dto.decode_request(BytesSubclass(raw)))
        self.assert_dto_error("invalid", lambda: self.dto.decode_request(b"\xff"))
        # Claude is now a first-class provider on the control plane (control_dto.rs
        # added it alongside codex), so a claude-scoped request DECODES.
        claude_start = self.dto.decode_request(
            self.requests["start"].replace(
                b'"provider":"codex"', b'"provider":"claude"'
            )
        )
        self.assertIs(
            claude_start.body.ticket.binding.scope.provider, self.dto.Provider.CLAUDE
        )
        # The ACP providers are expressible on the control plane too (whether the
        # product offers them is the coordinator's decision, not the wire's).
        for name, member in (
            (b"cursor", self.dto.Provider.CURSOR),
            (b"gemini", self.dto.Provider.GEMINI),
            (b"opencode", self.dto.Provider.OPENCODE),
        ):
            decoded = self.dto.decode_request(
                self.requests["start"].replace(
                    b'"provider":"codex"', b'"provider":"' + name + b'"'
                )
            )
            self.assertIs(decoded.body.ticket.binding.scope.provider, member)
        # An unknown provider is still rejected.
        for unknown in (b"grok", b"Gemini", b"cursor-agent", b""):
            self.assert_dto_error(
                "invalid",
                lambda unknown=unknown: self.dto.decode_request(
                    self.requests["start"].replace(
                        b'"provider":"codex"', b'"provider":"' + unknown + b'"'
                    )
                ),
            )
        nested_16 = b'{"v":1,"x":"[not depth]","a":' + b"[" * 15 + b"0" + b"]" * 15 + b"}"
        nested_17 = nested_16.replace(b"0", b"[0]", 1)
        self.dto._strict_parse(nested_16, 8192)
        self.assert_dto_error(
            "invalid", lambda: self.dto._strict_parse(nested_17, 8192)
        )
        padded = raw + b" " * (8192 - len(raw))
        self.assertEqual(len(padded), 8192)
        self.dto.decode_request(padded)
        self.assert_dto_error("invalid", lambda: self.dto.decode_request(padded + b" "))

    def test_parser_member_key_string_and_array_boundaries(self):
        members_32 = (
            "{" + ",".join('"k%02d":null' % index for index in range(32)) + "}"
        ).encode("ascii")
        members_33 = members_32[:-1] + b',"overflow":null}'
        key_64 = ('{"%s":null}' % ("k" * 64)).encode("ascii")
        key_65 = ('{"%s":null}' % ("k" * 65)).encode("ascii")
        string_1024 = ('{"x":"%s"}' % ("s" * 1024)).encode("ascii")
        string_1025 = ('{"x":"%s"}' % ("s" * 1025)).encode("ascii")
        array_32 = ('{"x":[%s]}' % ",".join("0" for _ in range(32))).encode(
            "ascii"
        )
        array_33 = array_32.replace(b"]}", b",0]}")

        for value in (members_32, key_64, string_1024, array_32):
            with self.subTest(accepted_length=len(value)):
                self.dto._strict_parse(value, 8192)
        for value in (members_33, key_65, string_1025, array_33):
            with self.subTest(rejected_length=len(value)):
                self.assert_dto_error(
                    "invalid", lambda value=value: self.dto._strict_parse(value, 8192)
                )

    def test_bounded_serialization_exact_response_extent_control(self):
        envelope_overhead = len(
            json.dumps(
                {"value": [""] * 16}, separators=(",", ":"), ensure_ascii=False
            ).encode("utf-8")
        )
        final_string_size = 16384 - envelope_overhead - (15 * 1024)
        self.assertEqual(final_string_size, 965)
        exact_value = ["s" * 1024] * 15 + ["s" * final_string_size]
        one_over_value = exact_value[:-1] + [exact_value[-1] + "s"]
        encoded = self.dto._encode_json(
            {"value": exact_value}, self.dto._RESPONSE_MAX
        )
        self.assertEqual(len(encoded), 16384)
        self.assert_dto_error(
            "invalid",
            lambda: self.dto._encode_json(
                {"value": one_over_value}, self.dto._RESPONSE_MAX
            ),
        )

    def test_every_invalid_literal_fixture_is_rejected(self):
        for item in self.fixture["invalid"]:
            decoder = (
                self.dto.decode_request
                if item["direction"] == "request"
                else self.dto.decode_response
            )
            with self.subTest(name=item["name"]):
                self.assert_dto_error(
                    item["expected_category"],
                    lambda item=item, decoder=decoder: decoder(
                        bytes.fromhex(item["payload_hex"])
                    ),
                )

    def test_invalid_errors_do_not_retain_rejected_exception_context(self):
        rejected = "SYNTHETIC-UNKNOWN-OP-CANARY"
        invalid_op = self.first_request_bytes.replace(
            b'"reconcile_page"', json.dumps(rejected).encode("ascii")
        )
        calls = (
            lambda: self.dto.decode_request(invalid_op),
            lambda: self.dto.decode_request(b"\xff"),
            lambda: self.dto._encode_json({"value": object()}, 8192),
        )
        for call in calls:
            with self.subTest(call=call):
                with self.assertRaises(self.dto.DtoError) as caught:
                    call()
                error = caught.exception
                rendered = "".join(
                    traceback.format_exception(type(error), error, error.__traceback__)
                )
                self.assertIsNone(error.__context__)
                self.assertIsNone(error.__cause__)
                self.assertNotIn(rejected, rendered)

    def test_response_extent_and_closed_variant_rules(self):
        raw = self.first_response_bytes
        padded = raw + b" " * (16384 - len(raw))
        self.assertEqual(len(padded), 16384)
        self.dto.decode_response(padded)
        self.assert_dto_error("invalid", lambda: self.dto.decode_response(padded + b" "))
        invalid = (
            raw.replace(b'"entries":[]', b'"entries":[],"error":null'),
            self.responses["observe_prepared"].replace(
                b'"account_label":"ChatGPT account"', b'"account_label":null,"challenge":null'
            ),
            self.responses["observe_awaiting_user"].replace(
                b"https://auth.openai.com/codex/device",
                b"https://auth.openai.com/codex/device?redirect=x",
            ),
            self.responses["check_finished_disconnected"].replace(
                b'"state":"disconnected"', b'"state":"disconnected","account_label":"x"'
            ),
            self.responses["cleanup_failed"].replace(
                b'"reason":"cleanup_failed"', b'"reason":"provider_error"'
            ),
        )
        for value in invalid:
            self.assert_dto_error("invalid", lambda value=value: self.dto.decode_response(value))

    def test_request_dependent_correlation(self):
        boot = self.dto.Id("3" * 32)
        request = self.request("settle")
        response = self.response("settle_committed")
        self.dto.validate_response(request, response, boot)
        for changed in (
            replace(response, coordinator=self.dto.Coordinator(self.dto.Counter("2"), self.dto.Id("1" * 32))),
            replace(response, supervisor_boot_id=self.dto.Id("f" * 32)),
            replace(response, sequence=self.dto.Counter("2")),
            replace(response, request_id=self.dto.Id("f" * 32)),
            replace(response, body=replace(response.body, receipt_id=self.dto.Id("f" * 32))),
            replace(response, body=replace(response.body, storage_handle=self.dto.Id("f" * 32))),
        ):
            self.assert_dto_error(
                "correlation",
                lambda changed=changed: self.dto.validate_response(request, changed, boot),
            )
        correlated_error = replace(
            self.response("error_busy"),
            request_id=request.request_id,
            operation=self.dto.Op.SETTLE_LOGIN,
        )
        wrong_op_error = replace(
            correlated_error, operation=self.dto.Op.OBSERVE_LOGIN
        )
        self.assert_dto_error(
            "correlation",
            lambda: self.dto.validate_response(request, wrong_op_error, boot),
        )
        check_request = self.request("check")
        check_response = self.response("check_queued")
        self.assert_dto_error(
            "correlation",
            lambda: self.dto.validate_response(
                check_request,
                replace(
                    check_response,
                    body=replace(check_response.body, check_id=self.dto.Id("f" * 32)),
                ),
                boot,
            ),
        )

    def test_login_snapshot_supplied_time_validation(self):
        binding = self.request("observe").body.binding
        snapshot = self.response("observe_awaiting_user").body
        expected = self.dto.UnixSec(2000000000)
        self.dto.validate_login_snapshot(
            binding, snapshot, expected, self.dto.UnixSec(1999999998)
        )
        self.assert_dto_error(
            "expired",
            lambda: self.dto.validate_login_snapshot(
                binding, snapshot, expected, self.dto.UnixSec(1999999999)
            ),
        )
        self.assert_dto_error(
            "correlation",
            lambda: self.dto.validate_login_snapshot(
                binding, snapshot, self.dto.UnixSec(2000000001), self.dto.UnixSec(0)
            ),
        )
        prepared = self.response("observe_prepared_null").body
        self.dto.validate_login_snapshot(
            binding, prepared, expected, self.dto.UnixSec(253402300799)
        )

        class SnapshotSubclass(self.dto.LoginSnapshot):
            pass

        self.assert_dto_error(
            "invalid",
            lambda: self.dto.validate_login_snapshot(
                binding, SnapshotSubclass(), expected, self.dto.UnixSec(0)
            ),
        )

    def test_reconcile_page_constraints(self):
        request = self.request("reconcile_first")
        response = self.response("reconcile_first_page")
        entry = self.dto.ReconcileEntry(
            self.dto.Id("c" * 32),
            self.request("check").body.ticket.binding,
            self.dto.Id("5" * 32),
            self.dto.Id("6" * 32),
            self.dto.RegistryPhase.PREPARED,
            self.dto.Id("9" * 32),
            self.dto.Id("8" * 32),
            None,
        )
        continuing = replace(
            response,
            body=self.dto.ReconcilePage(
                self.dto.Counter("1"), (entry,), self.dto.Id("c" * 32)
            ),
        )
        self.dto.validate_response(request, continuing, self.dto.Id("3" * 32))
        invalid_pages = (
            self.dto.ReconcilePage(self.dto.Counter("1"), (), self.dto.Id("c" * 32)),
            self.dto.ReconcilePage(
                self.dto.Counter("1"), (entry,), self.dto.Id("d" * 32)
            ),
        )
        for page in invalid_pages:
            self.assert_dto_error(
                "correlation",
                lambda page=page: self.dto.validate_response(
                    request, replace(response, body=page), self.dto.Id("3" * 32)
                ),
            )
        self.assert_dto_error(
            "invalid",
            lambda: self.dto.ReconcileEntry(
                self.dto.Id("d" * 32),
                entry.binding,
                entry.attempt_id,
                entry.operation_id,
                self.dto.RegistryPhase.PREPARED,
                None,
                self.dto.Id("8" * 32),
                None,
            ),
        )

    def test_reconcile_phase_nullability_matrix(self):
        phases = tuple(self.dto.RegistryPhase)
        for phase in phases:
            for handle in (False, True):
                for receipt in (False, True):
                    for cleanup in (False, True):
                        if phase is self.dto.RegistryPhase.ALLOCATION_PENDING:
                            valid = not handle and not receipt and not cleanup
                        elif phase is self.dto.RegistryPhase.PENDING_LOGIN:
                            valid = handle and not receipt and not cleanup
                        elif phase in (
                            self.dto.RegistryPhase.PREPARED,
                            self.dto.RegistryPhase.COMMITTED,
                        ):
                            valid = handle and receipt and not cleanup
                        elif phase is self.dto.RegistryPhase.CLEANUP_PENDING:
                            valid = cleanup and (not receipt or handle)
                        else:
                            valid = not receipt or handle
                        arguments = (1, phase, handle, receipt, cleanup)
                        with self.subTest(
                            phase=phase.value,
                            handle=handle,
                            receipt=receipt,
                            cleanup=cleanup,
                        ):
                            if valid:
                                self.reconcile_entry(*arguments)
                            else:
                                self.assert_dto_error(
                                    "invalid",
                                    lambda arguments=arguments: self.reconcile_entry(
                                        *arguments
                                    ),
                                )

    def test_reconcile_entry_count_order_revision_and_size_controls(self):
        boot = self.dto.Id("3" * 32)
        request = self.request("reconcile_first")
        response = self.response("reconcile_first_page")
        request_32 = replace(request, body=replace(request.body, limit=32))
        entries_32 = tuple(
            self.reconcile_entry(index + 1, self.dto.RegistryPhase.ALLOCATION_PENDING)
            for index in range(32)
        )
        page_32 = self.dto.ReconcilePage(self.dto.Counter("1"), entries_32, None)
        response_32 = replace(response, body=page_32)
        encoded_32 = self.dto.encode_response(response_32)
        self.assertEqual(len(self.dto.decode_response(encoded_32).body.entries), 32)
        self.dto.validate_response(request_32, response_32, boot)
        self.assert_dto_error(
            "invalid",
            lambda: self.dto.ReconcilePage(
                self.dto.Counter("1"),
                entries_32 + (self.reconcile_entry(33, self.dto.RegistryPhase.ALLOCATION_PENDING),),
                None,
            ),
        )

        continuation = replace(
            request,
            body=self.dto.ReconcilePageRequest(
                self.dto.Counter("1"), self.dto.Id("0" * 32), 32
            ),
        )
        one_entry = self.reconcile_entry(1, self.dto.RegistryPhase.ALLOCATION_PENDING)
        short_page = self.dto.ReconcilePage(
            self.dto.Counter("1"), (one_entry,), one_entry.entry_id
        )
        short_response = replace(response, body=short_page)
        self.dto.validate_response(continuation, short_response, boot)
        self.assert_dto_error(
            "correlation",
            lambda: self.dto.validate_response(
                continuation,
                replace(
                    short_response,
                    body=replace(short_page, snapshot_revision=self.dto.Counter("2")),
                ),
                boot,
            ),
        )

        duplicate = self.dto.ReconcilePage(
            self.dto.Counter("1"), (one_entry, one_entry), None
        )
        descending = self.dto.ReconcilePage(
            self.dto.Counter("1"),
            (
                self.reconcile_entry(2, self.dto.RegistryPhase.ALLOCATION_PENDING),
                one_entry,
            ),
            None,
        )
        for page in (duplicate, descending):
            self.assert_dto_error(
                "correlation",
                lambda page=page: self.dto.validate_response(
                    request_32, replace(response, body=page), boot
                ),
            )

        maximal_binding = self.dto.ConnectionBinding(
            self.dto.Scope(
                self.dto.OwnerId(2147483647),
                self.dto.OwnerId(2147483647),
                self.dto.Provider.CODEX,
            ),
            self.dto.Id("f" * 32),
            self.dto.Counter(str(I64_MAX)),
        )
        maximal_entries = tuple(
            replace(
                self.reconcile_entry(
                    index + 1,
                    self.dto.RegistryPhase.CLEANUP_PENDING,
                    handle=True,
                    receipt=True,
                    cleanup=True,
                ),
                binding=maximal_binding,
            )
            for index in range(32)
        )
        maximal_response = replace(
            response,
            coordinator=replace(
                response.coordinator, epoch=self.dto.Counter(str(I64_MAX))
            ),
            sequence=self.dto.Counter(str(I64_MAX)),
            body=self.dto.ReconcilePage(
                self.dto.Counter(str(I64_MAX)),
                maximal_entries,
                maximal_entries[-1].entry_id,
            ),
        )
        maximal_encoded = self.dto.encode_response(maximal_response)
        self.assertLess(len(maximal_encoded), 16384)
        self.assertEqual(
            len(self.dto.decode_response(maximal_encoded).body.entries), 32
        )

    def test_semantic_keys_distinguish_only_the_specified_fields(self):
        login = self.request("start").body.ticket
        self.assertEqual(self.dto.login_semantic_key(login), self.dto.login_semantic_key(login))
        changed_login = replace(login, expires_at=self.dto.UnixSec(login.expires_at.value + 1))
        self.assertNotEqual(
            self.dto.login_semantic_key(login), self.dto.login_semantic_key(changed_login)
        )
        cleanup = self.request("cleanup").body.ticket
        renewed = replace(
            cleanup,
            lease_owner=self.dto.Id("e" * 32),
            lease_expires_at=self.dto.UnixSec(cleanup.lease_expires_at.value + 1),
        )
        self.assertEqual(
            self.dto.cleanup_semantic_key(cleanup),
            self.dto.cleanup_semantic_key(renewed),
        )
        changed_target = replace(
            cleanup,
            target=self.dto.CleanupTarget(
                self.dto.CleanupKind.CANCEL, self.dto.Id("e" * 32)
            ),
        )
        self.assertNotEqual(
            self.dto.cleanup_semantic_key(cleanup),
            self.dto.cleanup_semantic_key(changed_target),
        )

    def test_channels_sequence_error_consumption_and_poisoning(self):
        coordinator = self.dto.Coordinator(self.dto.Counter("1"), self.dto.Id("1" * 32))
        boot = self.dto.Id("3" * 32)
        request1 = self.request("reconcile_first")
        response1 = self.response("reconcile_first_page")
        client = self.dto.ClientChannel(coordinator, boot)
        client.begin(request1)
        client.finish(response1)
        request2 = replace(request1, sequence=self.dto.Counter("2"), request_id=self.dto.Id("d" * 32))
        error2 = replace(
            self.response("error_busy"),
            sequence=self.dto.Counter("2"),
            request_id=self.dto.Id("d" * 32),
        )
        client.begin(request2)
        client.finish(error2)
        request3 = replace(request1, sequence=self.dto.Counter("3"), request_id=self.dto.Id("e" * 32))
        client.begin(request3)
        self.assert_dto_error("state", lambda: client.begin(request3))
        self.assert_dto_error("state", lambda: client.finish(response1))

        server = self.dto.ServerChannel(coordinator, boot)
        gap = replace(request1, sequence=self.dto.Counter("2"))
        self.assert_dto_error("sequence", lambda: server.accept(gap))
        self.assert_dto_error("state", lambda: server.accept(request1))
        server.poison()
        server.poison()

    def test_duplicate_reply_poisons_channel(self):
        request = self.request("reconcile_first")
        response = self.response("reconcile_first_page")
        coordinator = self.dto.Coordinator(self.dto.Counter("1"), self.dto.Id("1" * 32))
        channel = self.dto.ClientChannel(coordinator, self.dto.Id("3" * 32))
        channel.begin(request)
        channel.finish(response)
        self.assert_dto_error("state", lambda: channel.finish(response))
        self.assert_dto_error("state", lambda: channel.begin(request))

    def test_maximum_sequence_closes_without_wrapping(self):
        request = replace(
            self.request("reconcile_first"), sequence=self.dto.Counter(str(I64_MAX))
        )
        response = replace(
            self.response("reconcile_first_page"), sequence=self.dto.Counter(str(I64_MAX))
        )
        channel = self.dto.ClientChannel(request.coordinator, self.dto.Id("3" * 32))
        channel._next_sequence = I64_MAX
        channel.begin(request)
        channel.finish(response)
        self.assert_dto_error("state", lambda: channel.begin(request))

    def test_wire_composition_authenticates_before_dto_parse(self):
        wire = load_module("neev_supervisor_wire_for_dto", WIRE_PATH)
        key = bytes(range(32))
        context = wire.WireContext(bytes(range(32)), bytes(reversed(range(32))), b"3" * 16)
        frame = wire.encode_request(key, context, self.first_request_bytes)
        payload = wire.decode_request(key, context, frame)
        self.assertEqual(self.dto.decode_request(payload), self.request("reconcile_first"))
        invalid = b'{"v":1,"v":1}'
        authenticated_invalid = wire.encode_request(key, context, invalid)
        self.assertEqual(wire.decode_request(key, context, authenticated_invalid), invalid)
        self.assert_dto_error("invalid", lambda: self.dto.decode_request(invalid))

    def test_repr_and_errors_do_not_reflect_payload_canaries(self):
        values = (
            self.dto.Id("c" * 32),
            self.dto.Label("SYNTHETIC-LABEL-CANARY"),
            self.dto.UserCode("CANARY-CODE"),
            self.request("observe"),
            self.response("observe_awaiting_user"),
            self.dto.login_semantic_key(self.request("start").body.ticket),
            self.dto.ClientChannel(
                self.dto.Coordinator(self.dto.Counter("1"), self.dto.Id("1" * 32)),
                self.dto.Id("3" * 32),
            ),
        )
        for value in values:
            rendered = repr(value) + str(value)
            for canary in ("c" * 32, "SYNTHETIC-LABEL-CANARY", "CANARY-CODE"):
                self.assertNotIn(canary, rendered)
        with self.assertRaises(FrozenInstanceError):
            values[0].value = "d" * 32


if __name__ == "__main__":
    unittest.main()
