"""Chrome native messaging frame codec: 4-byte little-endian length + UTF-8 JSON.

The same codec is reused on the per-run unix socket so there is only one wire
format to audit.
"""

import json
import struct

MAX_MESSAGE_BYTES = 1048576
_HEADER = struct.Struct("<I")


class FrameError(Exception):
    """A frame was malformed, oversized, or truncated."""


def _read_exactly(stream, count):
    chunks = []
    remaining = count
    while remaining > 0:
        chunk = stream.read(remaining)
        if not chunk:
            return None
        chunks.append(chunk)
        remaining -= len(chunk)
    return b"".join(chunks)


def read_message(stream):
    """Read one frame. Returns None at a clean end of stream."""
    header = _read_exactly(stream, _HEADER.size)
    if header is None:
        return None
    (length,) = _HEADER.unpack(header)
    if length > MAX_MESSAGE_BYTES:
        raise FrameError("declared length %d exceeds the %d byte cap"
                         % (length, MAX_MESSAGE_BYTES))
    body = _read_exactly(stream, length)
    if body is None:
        raise FrameError("stream ended mid-message")
    try:
        return json.loads(body.decode("utf-8"))
    except (UnicodeDecodeError, ValueError) as exc:
        raise FrameError("undecodable frame: %s" % exc)


def write_message(stream, obj):
    body = json.dumps(obj, separators=(",", ":")).encode("utf-8")
    if len(body) > MAX_MESSAGE_BYTES:
        raise FrameError("message of %d bytes exceeds the cap" % len(body))
    stream.write(_HEADER.pack(len(body)))
    stream.write(body)
    stream.flush()
