"""Codex app-server MCP tool calls reach the trace.

`neev_codex_stream_adapter.translate` forwards `mcpToolCall` items as
`codex exec --json`'s own `mcp_tool_call` envelopes (one shape for both Codex
transports), bounded, with failure preserved. The acceptance parser is then fed
the REAL adapter output — no synthetic success."""

import io
import json
import os
import sys
import unittest

sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "operations"))

import neev_codex_stream_adapter as adapter  # noqa: E402
import neev_mcp_acceptance as acc  # noqa: E402

MARKER = "neev-accept-marker-3f2a"


def started(**item):
    base = {"type": "mcpToolCall", "id": "call_1", "server": "accept", "tool": "echo",
            "status": "inProgress", "arguments": {"text": MARKER}, "result": None, "error": None}
    base.update(item)
    return {"threadId": "t", "turnId": "u", "item": base}


def completed(**item):
    base = {"type": "mcpToolCall", "id": "call_1", "server": "accept", "tool": "echo",
            "status": "completed", "arguments": {"text": MARKER},
            "result": {"content": [{"type": "text", "text": "echo:" + MARKER}],
                       "structuredContent": None},
            "error": None, "durationMs": 12}
    base.update(item)
    return {"threadId": "t", "turnId": "u", "item": base}


def run(*notifications):
    out = io.StringIO()
    emit = adapter.Emitter(out)
    for method, params in notifications:
        adapter.translate(method, params, emit, {})
    return [json.loads(line) for line in out.getvalue().splitlines() if line.strip()]


class AdapterMcpTests(unittest.TestCase):
    def test_started_and_completed_are_forwarded_in_exec_shape(self):
        events = run(("item/started", started()), ("item/completed", completed()))
        self.assertEqual([e["type"] for e in events], ["item.started", "item.completed"])
        first, last = events[0]["item"], events[1]["item"]
        self.assertEqual(first, {"type": "mcp_tool_call", "id": "call_1", "server": "accept",
                                 "tool": "echo", "arguments": {"text": MARKER},
                                 "status": "in_progress"})
        self.assertEqual(last["id"], "call_1")
        self.assertEqual(last["status"], "completed")
        self.assertEqual(last["result"], {"content": [{"type": "text", "text": "echo:" + MARKER}]})
        self.assertIsNone(last["error"])
        self.assertNotIn("structuredContent", json.dumps(last))

    def test_failure_is_never_reported_as_success(self):
        for item in (
            {"status": "failed", "result": None,
             "error": {"message": "Integration accept unavailable: auth_failed"}},
            {"status": "completed", "result": {"content": [{"type": "text", "text": "no"}],
                                               "isError": True}},
            {"status": "completed", "error": {"message": "boom"}},
            {"status": "inProgress", "result": None},
            {"status": "weird", "result": None},
        ):
            with self.subTest(item=item):
                last = run(("item/completed", completed(**item)))[0]["item"]
                self.assertEqual(last["status"], "failed")
        failed = run(("item/completed", completed(
            status="failed", result=None, error={"message": "x" * 5000})))[0]["item"]
        self.assertLessEqual(len(failed["error"]["message"]), adapter.MAX_REASON_CHARS + 40)

    def test_sizes_are_bounded(self):
        big = "y" * (adapter.MAX_TOOL_PREVIEW_CHARS * 3)
        blocks = [{"type": "text", "text": big}] + [{"type": "image", "data": "A" * 9000}] * 30
        last = run(("item/completed", completed(
            result={"content": blocks}, arguments={"blob": "z" * 40000})))[0]["item"]
        self.assertLessEqual(len(last["result"]["content"]), adapter.MAX_MCP_RESULT_BLOCKS)
        self.assertTrue(last["result"]["content"][0]["text"].endswith("[Output preview truncated]"))
        self.assertEqual(last["result"]["content"][1], {"type": "image"})
        self.assertIsInstance(last["arguments"], str)
        self.assertLessEqual(len(last["arguments"]), adapter.MAX_MCP_ARGUMENTS_CHARS + 40)
        self.assertNotIn("A" * 100, json.dumps(last))

    def test_unsafe_names_are_dropped_not_forwarded(self):
        last = run(("item/completed", completed(server="bad server\n", tool="t" * 300)))[0]["item"]
        self.assertEqual((last["server"], last["tool"]), ("", ""))

    def test_other_items_are_unchanged(self):
        events = run(("item/started", {"item": {"type": "reasoning", "id": "r"}}),
                     ("item/completed", {"item": {"type": "agentMessage", "id": "m",
                                                  "text": "done"}}))
        self.assertEqual([e["type"] for e in events], ["item.completed"])
        self.assertEqual(events[0]["item"]["type"], "agent_message")


class AcceptanceSeesTheRealAdapterEvent(unittest.TestCase):
    def lines(self, *notifications):
        out = io.StringIO()
        emit = adapter.Emitter(out)
        for method, params in notifications:
            adapter.translate(method, params, emit, {})
        return out.getvalue().splitlines()

    def test_a_real_call_and_its_matching_result_pass(self):
        lines = self.lines(("item/started", started()), ("item/completed", completed()),
                           ("turn/completed", {"turn": {"status": "completed"}}))
        ok, detail = acc.parse_codex_stream(lines, "accept", "echo", MARKER)
        self.assertTrue(ok, detail)

    def test_a_failed_or_mismatched_call_does_not_pass(self):
        for item in ({"status": "failed", "result": None, "error": {"message": "denied"}},
                     {"result": {"content": [{"type": "text", "text": "echo:other"}]}},
                     {"arguments": {"text": "other"}},
                     {"server": "other"}):
            with self.subTest(item=item):
                lines = self.lines(("item/completed", completed(**item)),
                                   ("turn/completed", {"turn": {"status": "completed"}}))
                ok, _detail = acc.parse_codex_stream(lines, "accept", "echo", MARKER)
                self.assertFalse(ok)

    def test_no_call_at_all_does_not_pass(self):
        lines = self.lines(("turn/completed", {"turn": {"status": "completed"}}))
        self.assertFalse(acc.parse_codex_stream(lines, "accept", "echo", MARKER)[0])


if __name__ == "__main__":
    unittest.main()
