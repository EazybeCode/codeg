"""Pure authenticated binary framing for the Neev supervisor control channel."""

from dataclasses import dataclass
import hashlib
import hmac


_MAGIC = b"NEEVCTL1"
_HELLO_DOMAIN = b"neev-control-v1/hello\0"
_REQUEST_DOMAIN = b"neev-control-v1/request\0"
_RESPONSE_DOMAIN = b"neev-control-v1/response\0"
_KEY_SIZE = 32
_NONCE_SIZE = 32
_BOOT_ID_SIZE = 16
_MAC_SIZE = 32
_REQUEST_PAYLOAD_MAX = 8192
_RESPONSE_PAYLOAD_MAX = 16384
_FRAME_MAX = 65536
_CLIENT_HELLO_SIZE = len(_MAGIC) + _NONCE_SIZE
_SERVER_HELLO_PREFIX_SIZE = (
    len(_MAGIC) + _BOOT_ID_SIZE + _NONCE_SIZE + _NONCE_SIZE
)
_SERVER_HELLO_SIZE = _SERVER_HELLO_PREFIX_SIZE + _MAC_SIZE


class WireError(Exception):
    """A fixed, non-reflective wire validation failure."""

    def __init__(self, category):
        self.category = (
            category
            if type(category) is str and category in ("invalid", "auth")
            else "invalid"
        )
        super().__init__(self.category)

    def __repr__(self):
        return "WireError(%r)" % self.category


def _require_bytes(value, size=None):
    if type(value) is not bytes or (size is not None and len(value) != size):
        raise WireError("invalid")
    return value


@dataclass(frozen=True, repr=False)
class WireContext:
    client_nonce: bytes
    server_challenge: bytes
    supervisor_boot_id: bytes

    def __post_init__(self):
        _require_bytes(self.client_nonce, _NONCE_SIZE)
        _require_bytes(self.server_challenge, _NONCE_SIZE)
        _require_bytes(self.supervisor_boot_id, _BOOT_ID_SIZE)

    def __repr__(self):
        return "WireContext()"


def _require_key(key):
    return _require_bytes(key, _KEY_SIZE)


def _require_context(context):
    if type(context) is not WireContext:
        raise WireError("invalid")
    return context


def _require_payload(payload, maximum):
    _require_bytes(payload)
    if not payload or len(payload) > maximum:
        raise WireError("invalid")
    return payload


def _digest(key, *parts):
    value = hmac.new(key, digestmod=hashlib.sha256)
    for part in parts:
        value.update(part)
    return value.digest()


def encode_client_hello(client_nonce):
    client_nonce = _require_bytes(client_nonce, _NONCE_SIZE)
    return _MAGIC + client_nonce


def decode_client_hello(raw):
    raw = _require_bytes(raw)
    if len(raw) != _CLIENT_HELLO_SIZE or raw[: len(_MAGIC)] != _MAGIC:
        raise WireError("invalid")
    return raw[len(_MAGIC) :]


def encode_server_hello(key, context):
    key = _require_key(key)
    context = _require_context(context)
    prefix = (
        _MAGIC
        + context.supervisor_boot_id
        + context.client_nonce
        + context.server_challenge
    )
    return prefix + _digest(key, _HELLO_DOMAIN, prefix)


def decode_server_hello(key, expected_client_nonce, raw):
    key = _require_key(key)
    expected_client_nonce = _require_bytes(expected_client_nonce, _NONCE_SIZE)
    raw = _require_bytes(raw)
    if len(raw) != _SERVER_HELLO_SIZE or raw[: len(_MAGIC)] != _MAGIC:
        raise WireError("invalid")

    prefix = raw[:_SERVER_HELLO_PREFIX_SIZE]
    if not hmac.compare_digest(_digest(key, _HELLO_DOMAIN, prefix), raw[-_MAC_SIZE:]):
        raise WireError("auth")

    boot_start = len(_MAGIC)
    nonce_start = boot_start + _BOOT_ID_SIZE
    challenge_start = nonce_start + _NONCE_SIZE
    echoed_nonce = raw[nonce_start:challenge_start]
    if not hmac.compare_digest(expected_client_nonce, echoed_nonce):
        raise WireError("auth")
    return WireContext(
        echoed_nonce,
        raw[challenge_start:_SERVER_HELLO_PREFIX_SIZE],
        raw[boot_start:nonce_start],
    )


def encode_request(key, context, payload):
    key = _require_key(key)
    context = _require_context(context)
    payload = _require_payload(payload, _REQUEST_PAYLOAD_MAX)
    return _encode_frame(key, context, _REQUEST_DOMAIN, (), payload)


def decode_request(key, context, frame):
    key = _require_key(key)
    context = _require_context(context)
    frame = _require_bytes(frame)
    return _decode_frame(
        key, context, _REQUEST_DOMAIN, (), frame, _REQUEST_PAYLOAD_MAX
    )


def encode_response(key, context, request_payload, response_payload):
    key = _require_key(key)
    context = _require_context(context)
    request_payload = _require_payload(request_payload, _REQUEST_PAYLOAD_MAX)
    response_payload = _require_payload(response_payload, _RESPONSE_PAYLOAD_MAX)
    request_digest = hashlib.sha256(request_payload).digest()
    return _encode_frame(
        key, context, _RESPONSE_DOMAIN, (request_digest,), response_payload
    )


def decode_response(key, context, request_payload, frame):
    key = _require_key(key)
    context = _require_context(context)
    request_payload = _require_payload(request_payload, _REQUEST_PAYLOAD_MAX)
    frame = _require_bytes(frame)
    request_digest = hashlib.sha256(request_payload).digest()
    return _decode_frame(
        key,
        context,
        _RESPONSE_DOMAIN,
        (request_digest,),
        frame,
        _RESPONSE_PAYLOAD_MAX,
    )


def _encode_frame(key, context, domain, extra_parts, payload):
    length = len(payload).to_bytes(4, "big")
    body = length + payload
    frame = body + _digest(
        key,
        domain,
        context.client_nonce,
        context.server_challenge,
        context.supervisor_boot_id,
        *extra_parts,
        body,
    )
    if len(frame) > _FRAME_MAX:
        raise WireError("invalid")
    return frame


def _decode_frame(key, context, domain, extra_parts, frame, payload_maximum):
    if len(frame) < 4 + _MAC_SIZE or len(frame) > _FRAME_MAX:
        raise WireError("invalid")
    declared_length = int.from_bytes(frame[:4], "big")
    if declared_length == 0 or declared_length > payload_maximum:
        raise WireError("invalid")
    body_size = 4 + declared_length
    if len(frame) != body_size + _MAC_SIZE:
        raise WireError("invalid")

    body = frame[:body_size]
    expected_mac = _digest(
        key,
        domain,
        context.client_nonce,
        context.server_challenge,
        context.supervisor_boot_id,
        *extra_parts,
        body,
    )
    if not hmac.compare_digest(expected_mac, frame[body_size:]):
        raise WireError("auth")
    return frame[4:body_size]
