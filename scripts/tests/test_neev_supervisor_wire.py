"""Cross-language behavioral tests for the authenticated supervisor wire format."""

from dataclasses import FrozenInstanceError
import hashlib
import hmac
import importlib.util
import json
from pathlib import Path
import sys
import unittest


MODULE_PATH = Path(__file__).parents[1] / "operations" / "neev_supervisor_wire.py"
FIXTURE_PATH = Path(__file__).parent / "fixtures" / "neev-control-wire-v1.json"
REQUEST_LIMIT = 8192
RESPONSE_LIMIT = 16384


def load_module():
    spec = importlib.util.spec_from_file_location("neev_supervisor_wire", MODULE_PATH)
    if spec is None or spec.loader is None:
        raise RuntimeError("unable to load supervisor wire module")
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


class BytesSubclass(bytes):
    pass


class SupervisorWireTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.fixture = json.loads(FIXTURE_PATH.read_text(encoding="utf-8"))

    def setUp(self):
        self.assertTrue(MODULE_PATH.is_file(), "supervisor wire module is missing")
        self.wire = load_module()
        self.key = bytes.fromhex(self.fixture["key_hex"])
        self.client_nonce = bytes.fromhex(self.fixture["client_nonce_hex"])
        self.boot_id = bytes.fromhex(self.fixture["supervisor_boot_id_hex"])
        self.challenge = bytes.fromhex(self.fixture["server_challenge_hex"])
        self.request_payload = bytes.fromhex(self.fixture["request_payload_hex"])
        self.response_payload = bytes.fromhex(self.fixture["response_payload_hex"])
        self.context = self.wire.WireContext(
            self.client_nonce, self.challenge, self.boot_id
        )

    def assert_wire_error(self, category, operation, hidden=None):
        with self.assertRaises(self.wire.WireError) as caught:
            operation()
        self.assertEqual(caught.exception.category, category)
        self.assertEqual(str(caught.exception), category)
        self.assertEqual(repr(caught.exception), "WireError(%r)" % category)
        if hidden is not None:
            self.assertNotIn(hidden, str(caught.exception))
            self.assertNotIn(hidden, repr(caught.exception))

    def test_literal_fixture_round_trips_non_utf8_bytes_exactly(self):
        client_hello = bytes.fromhex(self.fixture["client_hello_hex"])
        server_hello = bytes.fromhex(self.fixture["server_hello_hex"])
        request_frame = bytes.fromhex(self.fixture["request_frame_hex"])
        response_frame = bytes.fromhex(self.fixture["response_frame_hex"])

        self.assertTrue(self.wire.encode_client_hello(self.client_nonce) == client_hello)
        self.assertTrue(self.wire.decode_client_hello(client_hello) == self.client_nonce)
        self.assertTrue(self.wire.encode_server_hello(self.key, self.context) == server_hello)
        decoded_context = self.wire.decode_server_hello(
            self.key, self.client_nonce, server_hello
        )
        self.assertTrue(
            self.wire.encode_request(self.key, decoded_context, self.request_payload)
            == request_frame
        )
        self.assertTrue(
            self.wire.decode_request(self.key, decoded_context, request_frame)
            == self.request_payload
        )
        self.assertTrue(
            self.wire.encode_response(
                self.key,
                decoded_context,
                self.request_payload,
                self.response_payload,
            )
            == response_frame
        )
        self.assertTrue(
            self.wire.decode_response(
                self.key, decoded_context, self.request_payload, response_frame
            )
            == self.response_payload
        )
        with self.assertRaises(UnicodeDecodeError):
            self.request_payload.decode("utf-8")

    def test_context_is_immutable_and_its_repr_suppresses_values(self):
        rendered = repr(self.context)
        for value in (self.client_nonce, self.challenge, self.boot_id):
            self.assertNotIn(value.hex(), rendered)
        with self.assertRaises(FrozenInstanceError):
            self.context.client_nonce = b"changed"

    def test_context_rejects_non_exact_bytes_and_lengths(self):
        valid = (self.client_nonce, self.challenge, self.boot_id)
        cases = (
            (bytearray(valid[0]), valid[1], valid[2]),
            (BytesSubclass(valid[0]), valid[1], valid[2]),
            (valid[0][:-1], valid[1], valid[2]),
            (valid[0], valid[1] + b"x", valid[2]),
            (valid[0], valid[1], valid[2][:-1]),
        )
        for values in cases:
            with self.subTest(lengths=tuple(len(value) for value in values)):
                self.assert_wire_error(
                    "invalid", lambda values=values: self.wire.WireContext(*values)
                )

    def test_every_public_function_rejects_invalid_key_or_context(self):
        server_hello = bytes.fromhex(self.fixture["server_hello_hex"])
        request_frame = bytes.fromhex(self.fixture["request_frame_hex"])
        response_frame = bytes.fromhex(self.fixture["response_frame_hex"])
        key_calls = (
            lambda key: self.wire.encode_server_hello(key, self.context),
            lambda key: self.wire.decode_server_hello(
                key, self.client_nonce, server_hello
            ),
            lambda key: self.wire.encode_request(
                key, self.context, self.request_payload
            ),
            lambda key: self.wire.decode_request(key, self.context, request_frame),
            lambda key: self.wire.encode_response(
                key, self.context, self.request_payload, self.response_payload
            ),
            lambda key: self.wire.decode_response(
                key, self.context, self.request_payload, response_frame
            ),
        )
        for call in key_calls:
            for key in (
                self.key[:-1],
                self.key + b"x",
                bytearray(self.key),
                BytesSubclass(self.key),
            ):
                with self.subTest(call=call, key_type=type(key), key_len=len(key)):
                    self.assert_wire_error("invalid", lambda: call(key))

        context_calls = (
            lambda context: self.wire.encode_server_hello(self.key, context),
            lambda context: self.wire.encode_request(
                self.key, context, self.request_payload
            ),
            lambda context: self.wire.decode_request(
                self.key, context, request_frame
            ),
            lambda context: self.wire.encode_response(
                self.key, context, self.request_payload, self.response_payload
            ),
            lambda context: self.wire.decode_response(
                self.key, context, self.request_payload, response_frame
            ),
        )
        for call in context_calls:
            with self.subTest(call=call):
                self.assert_wire_error("invalid", lambda: call(object()))

    def test_public_byte_inputs_require_exact_bytes(self):
        client_hello = bytes.fromhex(self.fixture["client_hello_hex"])
        server_hello = bytes.fromhex(self.fixture["server_hello_hex"])
        request_frame = bytes.fromhex(self.fixture["request_frame_hex"])
        response_frame = bytes.fromhex(self.fixture["response_frame_hex"])
        calls = (
            (lambda value: self.wire.encode_client_hello(value), self.client_nonce),
            (lambda value: self.wire.decode_client_hello(value), client_hello),
            (
                lambda value: self.wire.decode_server_hello(
                    self.key, self.client_nonce, value
                ),
                server_hello,
            ),
            (
                lambda value: self.wire.decode_server_hello(
                    self.key, value, server_hello
                ),
                self.client_nonce,
            ),
            (
                lambda value: self.wire.encode_request(self.key, self.context, value),
                self.request_payload,
            ),
            (
                lambda value: self.wire.decode_request(self.key, self.context, value),
                request_frame,
            ),
            (
                lambda value: self.wire.encode_response(
                    self.key, self.context, value, self.response_payload
                ),
                self.request_payload,
            ),
            (
                lambda value: self.wire.encode_response(
                    self.key, self.context, self.request_payload, value
                ),
                self.response_payload,
            ),
            (
                lambda value: self.wire.decode_response(
                    self.key, self.context, self.request_payload, value
                ),
                response_frame,
            ),
            (
                lambda value: self.wire.decode_response(
                    self.key, self.context, value, response_frame
                ),
                self.request_payload,
            ),
        )
        for call, valid_value in calls:
            for value in (
                bytearray(valid_value),
                BytesSubclass(valid_value),
                "bytes",
            ):
                with self.subTest(call=call, value_type=type(value)):
                    self.assert_wire_error("invalid", lambda: call(value))

    def test_client_hello_rejects_wrong_lengths_and_magic(self):
        hello = bytes.fromhex(self.fixture["client_hello_hex"])
        for nonce in (self.client_nonce[:-1], self.client_nonce + b"x"):
            self.assert_wire_error(
                "invalid", lambda nonce=nonce: self.wire.encode_client_hello(nonce)
            )
        for raw in (hello[:-1], hello + b"x", b"BADMAGIC" + hello[8:]):
            self.assert_wire_error(
                "invalid", lambda raw=raw: self.wire.decode_client_hello(raw)
            )

    def test_server_hello_validates_extent_magic_mac_then_echoed_nonce(self):
        hello = bytes.fromhex(self.fixture["server_hello_hex"])
        malformed = (hello[:-1], hello + b"x", b"BADMAGIC" + hello[8:])
        for raw in malformed:
            self.assert_wire_error(
                "invalid",
                lambda raw=raw: self.wire.decode_server_hello(
                    self.key, self.client_nonce, raw
                ),
            )

        for index in (8, 24, 56, len(hello) - 1):
            forged = bytearray(hello)
            forged[index] ^= 1
            self.assert_wire_error(
                "auth",
                lambda forged=bytes(forged): self.wire.decode_server_hello(
                    self.key, self.client_nonce, forged
                ),
            )

        other_nonce = bytes(reversed(self.client_nonce))
        other_context = self.wire.WireContext(
            other_nonce, self.challenge, self.boot_id
        )
        valid_for_other_nonce = self.wire.encode_server_hello(self.key, other_context)
        self.assert_wire_error(
            "auth",
            lambda: self.wire.decode_server_hello(
                self.key, self.client_nonce, valid_for_other_nonce
            ),
        )

    def test_server_hello_rejects_a_mac_from_another_domain(self):
        hello = bytes.fromhex(self.fixture["server_hello_hex"])
        prefix = hello[:-32]
        reflected = prefix + hmac.new(
            self.key, b"neev-control-v1/request\0" + prefix, hashlib.sha256
        ).digest()
        self.assert_wire_error(
            "auth",
            lambda: self.wire.decode_server_hello(
                self.key, self.client_nonce, reflected
            ),
        )

    def test_request_payload_limits_accept_lower_and_exact_bounds(self):
        for size in (1, REQUEST_LIMIT):
            payload = b"r" * size
            frame = self.wire.encode_request(self.key, self.context, payload)
            self.assertTrue(
                self.wire.decode_request(self.key, self.context, frame) == payload
            )
        for payload in (b"", b"r" * (REQUEST_LIMIT + 1)):
            self.assert_wire_error(
                "invalid",
                lambda payload=payload: self.wire.encode_request(
                    self.key, self.context, payload
                ),
            )

    def test_response_payload_limits_accept_lower_and_exact_bounds(self):
        for size in (1, RESPONSE_LIMIT):
            payload = b"s" * size
            frame = self.wire.encode_response(
                self.key, self.context, self.request_payload, payload
            )
            self.assertTrue(
                self.wire.decode_response(
                    self.key, self.context, self.request_payload, frame
                )
                == payload
            )
        for payload in (b"", b"s" * (RESPONSE_LIMIT + 1)):
            self.assert_wire_error(
                "invalid",
                lambda payload=payload: self.wire.encode_response(
                    self.key, self.context, self.request_payload, payload
                ),
            )

    def test_response_request_digest_input_obeys_request_limits(self):
        frame = bytes.fromhex(self.fixture["response_frame_hex"])
        for request_payload in (b"", b"r" * (REQUEST_LIMIT + 1)):
            self.assert_wire_error(
                "invalid",
                lambda request_payload=request_payload: self.wire.encode_response(
                    self.key, self.context, request_payload, self.response_payload
                ),
            )
            self.assert_wire_error(
                "invalid",
                lambda request_payload=request_payload: self.wire.decode_response(
                    self.key, self.context, request_payload, frame
                ),
            )

    def test_request_decoder_rejects_zero_overflow_truncation_and_trailing_bytes(self):
        frame = bytes.fromhex(self.fixture["request_frame_hex"])
        malformed = (
            b"\x00\x00\x00\x00" + b"x" * 32,
            b"\xff\xff\xff\xff" + b"x" * 32,
            frame[:-1],
            frame + b"x",
            b"x" * 65537,
        )
        for raw in malformed:
            self.assert_wire_error(
                "invalid",
                lambda raw=raw: self.wire.decode_request(
                    self.key, self.context, raw
                ),
            )

    def test_response_decoder_rejects_zero_overflow_truncation_and_trailing_bytes(self):
        frame = bytes.fromhex(self.fixture["response_frame_hex"])
        malformed = (
            b"\x00\x00\x00\x00" + b"x" * 32,
            b"\xff\xff\xff\xff" + b"x" * 32,
            frame[:-1],
            frame + b"x",
            b"x" * 65537,
        )
        for raw in malformed:
            self.assert_wire_error(
                "invalid",
                lambda raw=raw: self.wire.decode_response(
                    self.key, self.context, self.request_payload, raw
                ),
            )

    def test_request_authentication_binds_domain_context_length_payload_and_mac(self):
        frame = bytes.fromhex(self.fixture["request_frame_hex"])
        for index in (4, len(frame) - 1):
            changed = bytearray(frame)
            changed[index] ^= 1
            self.assert_wire_error(
                "auth",
                lambda changed=bytes(changed): self.wire.decode_request(
                    self.key, self.context, changed
                ),
            )

        wrong_domain = frame[:-32] + hmac.new(
            self.key,
            b"neev-control-v1/response\0"
            + self.client_nonce
            + self.challenge
            + self.boot_id
            + frame[:-32],
            hashlib.sha256,
        ).digest()
        self.assert_wire_error(
            "auth",
            lambda: self.wire.decode_request(self.key, self.context, wrong_domain),
        )
        for context in self._other_contexts():
            self.assert_wire_error(
                "auth",
                lambda context=context: self.wire.decode_request(
                    self.key, context, frame
                ),
            )

    def test_response_authentication_binds_context_request_digest_payload_and_mac(self):
        frame = bytes.fromhex(self.fixture["response_frame_hex"])
        for index in (4, len(frame) - 1):
            changed = bytearray(frame)
            changed[index] ^= 1
            self.assert_wire_error(
                "auth",
                lambda changed=bytes(changed): self.wire.decode_response(
                    self.key, self.context, self.request_payload, changed
                ),
            )
        self.assert_wire_error(
            "auth",
            lambda: self.wire.decode_response(
                self.key, self.context, self.request_payload + b"x", frame
            ),
        )
        for context in self._other_contexts():
            self.assert_wire_error(
                "auth",
                lambda context=context: self.wire.decode_response(
                    self.key, context, self.request_payload, frame
                ),
            )

    def test_wire_errors_never_echo_arbitrary_values(self):
        hidden = "invented-sensitive-value"
        error = self.wire.WireError(hidden)
        self.assertEqual(error.category, "invalid")
        self.assertEqual(str(error), "invalid")
        self.assertEqual(repr(error), "WireError('invalid')")
        self.assertNotIn(hidden, repr(error))

    def _other_contexts(self):
        return (
            self.wire.WireContext(
                bytes(reversed(self.client_nonce)), self.challenge, self.boot_id
            ),
            self.wire.WireContext(
                self.client_nonce, bytes(reversed(self.challenge)), self.boot_id
            ),
            self.wire.WireContext(
                self.client_nonce, self.challenge, bytes(reversed(self.boot_id))
            ),
        )


if __name__ == "__main__":
    unittest.main()
