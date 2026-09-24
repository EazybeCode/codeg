import io
import json
import struct
import unittest

from neev_browser.framing import (MAX_MESSAGE_BYTES, FrameError, read_message,
                                  write_message)


def framed(obj):
    raw = json.dumps(obj).encode("utf-8")
    return struct.pack("<I", len(raw)) + raw


class FramingTest(unittest.TestCase):
    def test_round_trip(self):
        out = io.BytesIO()
        write_message(out, {"type": "ping", "n": 1})
        self.assertEqual(read_message(io.BytesIO(out.getvalue())),
                         {"type": "ping", "n": 1})

    def test_eof_returns_none(self):
        self.assertIsNone(read_message(io.BytesIO(b"")))

    def test_truncated_body_is_an_error_not_a_short_read(self):
        stream = io.BytesIO(struct.pack("<I", 64) + b"{}")
        with self.assertRaises(FrameError):
            read_message(stream)

    def test_oversized_declared_length_is_rejected_before_reading(self):
        stream = io.BytesIO(struct.pack("<I", MAX_MESSAGE_BYTES + 1))
        with self.assertRaises(FrameError):
            read_message(stream)

    def test_non_json_body_is_a_frame_error(self):
        stream = io.BytesIO(struct.pack("<I", 3) + b"not")
        with self.assertRaises(FrameError):
            read_message(stream)

    def test_two_messages_in_one_buffer_are_read_in_order(self):
        stream = io.BytesIO(framed({"i": 1}) + framed({"i": 2}))
        self.assertEqual(read_message(stream)["i"], 1)
        self.assertEqual(read_message(stream)["i"], 2)

    def test_writing_an_oversized_message_is_refused(self):
        with self.assertRaises(FrameError):
            write_message(io.BytesIO(), {"blob": "x" * (MAX_MESSAGE_BYTES + 1)})


if __name__ == "__main__":
    unittest.main()
