"""Strict DTO and channel tests for the Neev supervisor control protocol."""

from dataclasses import FrozenInstanceError, replace
import importlib.util
import json
from pathlib import Path
import sys
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
        self.assert_dto_error(
            "invalid",
            lambda: self.dto.decode_request(
                self.requests["start"].replace(
                    b'"provider":"codex"', b'"provider":"claude"'
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
