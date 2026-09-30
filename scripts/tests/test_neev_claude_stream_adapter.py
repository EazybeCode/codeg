"""Tests for the Claude streaming adapter with interrupt-and-continue steering.

The adapter passes through native Claude events unchanged. For steering, it uses:
- control_request {type:"control_request", request_id, request:{subtype:"interrupt"}}
- control_response {type:"control_response", response:{request_id, subtype:"success"|"error", response:{still_queued:[]}}}
- user replay for acknowledgment

SDK wire format verified from claude-agent-sdk-python _send_control_request.
Tests use a fake Claude CLI that implements the actual SDK wire protocol.
"""

import io
import json
import os
import stat
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from pathlib import Path

OPERATIONS = Path(__file__).parents[1] / "operations"
sys.path.insert(0, str(OPERATIONS))

import neev_claude_stream_adapter as adapter  # noqa: E402

ADAPTER = str(OPERATIONS / "neev_claude_stream_adapter.py")


# Fake Claude CLI implementing actual SDK wire protocol
FAKE_CLAUDE = '''#!/usr/bin/env python3
"""Fake Claude CLI implementing actual SDK stream-json wire protocol."""
import json
import os
import sys
import time

MODE = os.environ.get("FAKE_MODE", "ok")

if MODE == "dies":
    # Exits before any stream-json, explaining itself on stderr only.
    sys.stderr.write("Invalid API key \u00b7 Please run /login\\n")
    sys.stderr.flush()
    sys.exit(1)

def send(payload):
    sys.stdout.write(json.dumps(payload) + "\\n")
    sys.stdout.flush()

def send_system_init():
    send({
        "type": "system",
        "subtype": "init",
        "session_id": "test-session-123",
        "tools": ["Bash", "Read", "Write"],
        "mcp_servers": [],
        "model": "claude-sonnet-4-20250514",
        "cwd": os.getcwd(),
        "capabilities": ["interrupt_receipt_v1"],
    })

def send_stream_event(event):
    send({"type": "stream_event", "event": event})

def send_text_delta(index, text):
    send_stream_event({
        "type": "content_block_delta",
        "index": index,
        "delta": {"type": "text_delta", "text": text}
    })

def send_assistant_message(msg_id, content):
    send({
        "type": "assistant",
        "message": {
            "id": msg_id,
            "role": "assistant",
            "content": content,
        }
    })

def send_result(text, is_error=False, subtype="success"):
    result = {
        "type": "result",
        "subtype": subtype,
        "uuid": "result-uuid-123",
        "session_id": "test-session-123",
        "is_error": is_error,
        "result": text,
        "total_cost_usd": 0.001,
        "duration_ms": 100,
        "num_turns": 1,
    }
    send(result)

def send_user_replay(uuid_val, text):
    """Echo user message back (--replay-user-messages)."""
    send({
        "type": "user",
        "uuid": uuid_val,
        "session_id": "test-session-123",
        "message": {"role": "user", "content": text},
        "parent_tool_use_id": None,
        "isReplay": True,
    })

def send_control_response(request_id, still_queued=None, error=None):
    """Send control_response for interrupt.

    SDK wire format (from claude-agent-sdk-python):
      {type:"control_response", response:{request_id, subtype:"success"|"error", response:{still_queued:[]}, error:"str"}}
    """
    if error:
        send({
            "type": "control_response",
            "response": {
                "request_id": request_id,
                "subtype": "error",
                "error": error
            }
        })
    else:
        send({
            "type": "control_response",
            "response": {
                "request_id": request_id,
                "subtype": "success",
                "response": {"still_queued": still_queued or []}
            }
        })

if MODE == "silent":
    time.sleep(30)
    sys.exit(0)

if MODE == "immediate_error":
    send_system_init()
    send_result("API error", is_error=True, subtype="error_during_execution")
    sys.exit(0)

if MODE == "no_init":
    time.sleep(30)
    sys.exit(0)

# Read initial user message from stdin
initial_msg = None
for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    try:
        msg = json.loads(line)
        if msg.get("type") == "user":
            initial_msg = msg
            break
    except:
        pass

if not initial_msg:
    sys.exit(1)

# Normal flow
send_system_init()
send_text_delta(0, "Hello")
send_text_delta(0, " world")
send_assistant_message("msg-1", [{"type": "text", "text": "Hello world"}])

if MODE == "with_tool":
    send_stream_event({
        "type": "content_block_start",
        "index": 1,
        "content_block": {"type": "tool_use", "id": "tool-1", "name": "Bash"}
    })
    send_stream_event({
        "type": "content_block_delta",
        "index": 1,
        "delta": {"type": "input_json_delta", "partial_json": '{"command": "ls"}'}
    })
    send_stream_event({"type": "content_block_stop", "index": 1})

if MODE == "steer":
    # Wait for steering input (user message + control_request)
    import select
    pending_user = None
    while True:
        readable, _, _ = select.select([sys.stdin], [], [], 1.0)
        if readable:
            try:
                line = sys.stdin.readline()
                if not line:
                    break
                msg = json.loads(line.strip())
                msg_type = msg.get("type")

                if msg_type == "user":
                    # Queue the user message
                    pending_user = msg

                elif msg_type == "control_request":
                    req = msg.get("request", {})
                    # SDK uses 'subtype' not 'type'
                    if req.get("subtype") == "interrupt":
                        request_id = msg.get("request_id")
                        if pending_user:
                            # UUID is queued
                            uuid_val = pending_user.get("uuid")
                            send_control_response(request_id, still_queued=[uuid_val])
                            # Echo the user message (replay)
                            text = pending_user.get("message", {}).get("content", "")
                            send_user_replay(uuid_val, text)
                            pending_user = None
                            # Continue with steered output
                            send_text_delta(0, " [steered]")
                        else:
                            send_control_response(request_id, still_queued=[])
                        break
            except Exception as e:
                break
        else:
            break

if MODE == "steer_reject":
    # Simulate dropped message (not in still_queued)
    import select
    pending_user = None
    while True:
        readable, _, _ = select.select([sys.stdin], [], [], 1.0)
        if readable:
            try:
                line = sys.stdin.readline()
                if not line:
                    break
                msg = json.loads(line.strip())
                msg_type = msg.get("type")

                if msg_type == "user":
                    pending_user = msg

                elif msg_type == "control_request":
                    req = msg.get("request", {})
                    # SDK uses 'subtype' not 'type'
                    if req.get("subtype") == "interrupt":
                        request_id = msg.get("request_id")
                        # Empty still_queued = message was dropped
                        send_control_response(request_id, still_queued=[])
                        break
            except:
                break
        else:
            break

if MODE == "steer_error":
    # Simulate interrupt error
    import select
    while True:
        readable, _, _ = select.select([sys.stdin], [], [], 1.0)
        if readable:
            try:
                line = sys.stdin.readline()
                if not line:
                    break
                msg = json.loads(line.strip())
                if msg.get("type") == "control_request":
                    request_id = msg.get("request_id")
                    send_control_response(request_id, error="interrupt failed")
                    break
            except:
                break
        else:
            break

if MODE == "echo_before_response":
    # Edge case: echo arrives BEFORE control_response
    import select
    pending_user = None
    while True:
        readable, _, _ = select.select([sys.stdin], [], [], 1.0)
        if readable:
            try:
                line = sys.stdin.readline()
                if not line:
                    break
                msg = json.loads(line.strip())
                msg_type = msg.get("type")

                if msg_type == "user":
                    pending_user = msg

                elif msg_type == "control_request":
                    request_id = msg.get("request_id")
                    if pending_user:
                        uuid_val = pending_user.get("uuid")
                        text = pending_user.get("message", {}).get("content", "")
                        # Echo FIRST
                        send_user_replay(uuid_val, text)
                        # Then control_response (shouldn't matter)
                        send_control_response(request_id, still_queued=[uuid_val])
                        pending_user = None
                    break
            except:
                break
        else:
            break

if MODE == "oversized":
    # Send oversized line
    send({"type": "text", "data": "x" * (65 * 1024 * 1024)})

send_result("Hello world")
'''


def fake_claude(directory):
    path = Path(directory) / "fake_claude.py"
    path.write_text(FAKE_CLAUDE)
    path.chmod(path.stat().st_mode | stat.S_IEXEC | stat.S_IXGRP | stat.S_IXOTH)
    return str(path)


class ArgvTests(unittest.TestCase):
    def parse(self, extra):
        return adapter._parse(["--claude", "/usr/local/bin/claude"] + extra)

    def test_prompt_is_taken_literally(self):
        for prompt in ("--help", "-p nested", "  spaced  ", "--"):
            with self.subTest(prompt=prompt):
                self.assertEqual(self.parse(["--", prompt]).prompt, prompt)

    def test_claude_argv_uses_empty_p_flag(self):
        """Prompt is sent via stdin; -p "" activates print mode without duplication."""
        args = self.parse(["--", "hello"])
        argv = adapter.claude_argv(args)
        # -p "" keeps print mode active; actual prompt via stdin
        self.assertIn("-p", argv)
        idx = argv.index("-p")
        self.assertEqual(argv[idx + 1], "")  # Empty string
        # Actual prompt NOT in argv
        self.assertNotIn("hello", argv)
        self.assertIn("--output-format", argv)
        self.assertIn("stream-json", argv)
        self.assertIn("--input-format", argv)
        self.assertIn("--replay-user-messages", argv)

    def test_model_and_effort_are_preserved(self):
        args = self.parse(["--model", "opus", "--effort", "high", "--", "hi"])
        argv = adapter.claude_argv(args)
        self.assertIn("--model", argv)
        self.assertIn("opus", argv)
        self.assertIn("--effort", argv)
        self.assertIn("high", argv)

    def test_append_system_prompt_is_preserved(self):
        args = self.parse(["--append-system-prompt", "Be helpful", "--", "hi"])
        argv = adapter.claude_argv(args)
        self.assertIn("--append-system-prompt", argv)
        self.assertIn("Be helpful", argv)

    def test_verbose_flag_always_present(self):
        """--verbose is REQUIRED when using -p "" with stream-json output."""
        args = self.parse(["--", "hi"])
        argv = adapter.claude_argv(args)
        self.assertIn("--verbose", argv)

    def test_include_partial_messages_flag_is_preserved(self):
        args = self.parse(["--include-partial-messages", "--", "hi"])
        argv = adapter.claude_argv(args)
        self.assertIn("--include-partial-messages", argv)

    def test_settings_is_preserved(self):
        args = self.parse(["--settings", "/path/to/settings.json", "--", "hi"])
        argv = adapter.claude_argv(args)
        self.assertIn("--settings", argv)
        self.assertIn("/path/to/settings.json", argv)

    def test_enable_steering_flag_parsed(self):
        args = self.parse(["--enable-steering", "--", "hi"])
        self.assertTrue(args.enable_steering)

    def test_enable_steering_defaults_false(self):
        args = self.parse(["--", "hi"])
        self.assertFalse(args.enable_steering)


class ProtocolTests(unittest.TestCase):
    def test_build_user_message_structure(self):
        msg = adapter._build_user_message("hello world", "uuid-123")
        self.assertEqual(msg["type"], "user")
        self.assertEqual(msg["uuid"], "uuid-123")
        self.assertEqual(msg["message"]["role"], "user")
        self.assertEqual(msg["message"]["content"], "hello world")
        self.assertIsNone(msg["parent_tool_use_id"])

    def test_build_control_request_structure(self):
        req = adapter._build_control_request("ctrl-1")
        self.assertEqual(req["type"], "control_request")
        self.assertEqual(req["request_id"], "ctrl-1")
        # SDK uses 'subtype' not 'type' inside request
        self.assertEqual(req["request"]["subtype"], "interrupt")
        # Must NOT have cancel_queued
        self.assertNotIn("cancel_queued", req["request"])


class SteerStateTests(unittest.TestCase):
    def test_register_steer_returns_control_id(self):
        emit = adapter.Emitter(io.StringIO())
        state = adapter.SteerState(emit)
        control_id = state.register_steer("uuid-1", "req-1")
        self.assertIsNotNone(control_id)
        self.assertTrue(control_id.startswith("neev-ctrl-"))

    def test_register_steer_after_finished_returns_none(self):
        emit = adapter.Emitter(io.StringIO())
        state = adapter.SteerState(emit)
        state.mark_finished()
        control_id = state.register_steer("uuid-1", "req-1")
        self.assertIsNone(control_id)

    def test_register_steer_bounded(self):
        emit = adapter.Emitter(io.StringIO())
        state = adapter.SteerState(emit)
        # Fill to max
        for i in range(adapter.MAX_PENDING_STEERS):
            result = state.register_steer(f"uuid-{i}", f"req-{i}")
            self.assertIsNotNone(result)
        # Next should fail
        result = state.register_steer("uuid-overflow", "req-overflow")
        self.assertIsNone(result)

    def test_handle_control_response_success(self):
        output = io.StringIO()
        emit = adapter.Emitter(output)
        state = adapter.SteerState(emit)
        control_id = state.register_steer("uuid-1", "req-1")

        # SDK wire format: {type:"control_response", response:{request_id, subtype:"success", response:{still_queued:[]}}}
        response = {
            "type": "control_response",
            "response": {
                "request_id": control_id,
                "subtype": "success",
                "response": {"still_queued": ["uuid-1"]}
            }
        }
        handled = state.handle_control_response(response)
        self.assertTrue(handled)
        # No ack yet - waiting for echo
        self.assertEqual(output.getvalue(), "")

    def test_handle_control_response_not_queued(self):
        output = io.StringIO()
        emit = adapter.Emitter(output)
        state = adapter.SteerState(emit)
        control_id = state.register_steer("uuid-1", "req-1")

        # SDK wire format: empty still_queued = message was dropped
        response = {
            "type": "control_response",
            "response": {
                "request_id": control_id,
                "subtype": "success",
                "response": {"still_queued": []}
            }
        }
        handled = state.handle_control_response(response)
        self.assertTrue(handled)
        # Should reject
        lines = [json.loads(l) for l in output.getvalue().strip().split("\n") if l]
        self.assertEqual(len(lines), 1)
        self.assertEqual(lines[0]["type"], "neev.steer")
        self.assertFalse(lines[0]["accepted"])
        self.assertEqual(lines[0]["reason"], "message not queued")

    def test_handle_control_response_error(self):
        output = io.StringIO()
        emit = adapter.Emitter(output)
        state = adapter.SteerState(emit)
        control_id = state.register_steer("uuid-1", "req-1")

        # SDK wire format: subtype:"error" with error message
        response = {
            "type": "control_response",
            "response": {
                "request_id": control_id,
                "subtype": "error",
                "error": "interrupt failed"
            }
        }
        handled = state.handle_control_response(response)
        self.assertTrue(handled)
        lines = [json.loads(l) for l in output.getvalue().strip().split("\n") if l]
        self.assertEqual(len(lines), 1)
        self.assertFalse(lines[0]["accepted"])

    def test_confirm_echo_emits_acceptance(self):
        output = io.StringIO()
        emit = adapter.Emitter(output)
        state = adapter.SteerState(emit)
        state.register_steer("uuid-1", "req-1")
        result = state.confirm_echo("uuid-1")
        self.assertTrue(result)
        lines = [json.loads(l) for l in output.getvalue().strip().split("\n") if l]
        self.assertEqual(len(lines), 1)
        self.assertEqual(lines[0]["type"], "neev.steer")
        self.assertEqual(lines[0]["requestId"], "req-1")
        self.assertTrue(lines[0]["accepted"])

    def test_should_suppress_result(self):
        emit = adapter.Emitter(io.StringIO())
        state = adapter.SteerState(emit)
        # Register increments suppress count
        state.register_steer("uuid-1", "req-1")
        self.assertTrue(state.should_suppress_result())
        # Second call returns false
        self.assertFalse(state.should_suppress_result())


class SteerInputTests(unittest.TestCase):
    def test_parse_valid_input(self):
        line = '{"requestId": "abc123", "text": "follow up"}'
        req_id, text = adapter._parse_steer_input(line)
        self.assertEqual(req_id, "abc123")
        self.assertEqual(text, "follow up")

    def test_parse_missing_request_id(self):
        with self.assertRaises(ValueError):
            adapter._parse_steer_input('{"text": "hi"}')

    def test_parse_empty_request_id(self):
        with self.assertRaises(ValueError):
            adapter._parse_steer_input('{"requestId": "", "text": "hi"}')

    def test_parse_missing_text(self):
        with self.assertRaises(ValueError):
            adapter._parse_steer_input('{"requestId": "abc"}')

    def test_parse_text_too_large(self):
        big = "x" * (adapter.STEER_MAX_TEXT_BYTES + 1)
        with self.assertRaises(ValueError):
            adapter._parse_steer_input(json.dumps({"requestId": "a", "text": big}))

    def test_parse_line_too_large(self):
        big_line = " " * (adapter.STEER_MAX_LINE_BYTES + 1)
        with self.assertRaises(ValueError):
            adapter._parse_steer_input(big_line)


class EmitterTests(unittest.TestCase):
    def test_passthrough_preserves_line(self):
        output = io.StringIO()
        emit = adapter.Emitter(output)
        emit.passthrough('{"type": "assistant", "data": "test"}')
        self.assertEqual(output.getvalue().strip(), '{"type": "assistant", "data": "test"}')

    def test_passthrough_adds_newline(self):
        output = io.StringIO()
        emit = adapter.Emitter(output)
        emit.passthrough('{"type": "test"}')
        self.assertTrue(output.getvalue().endswith("\n"))

    def test_steer_accepted(self):
        output = io.StringIO()
        emit = adapter.Emitter(output)
        emit.steer_accepted("req-123")
        result = json.loads(output.getvalue().strip())
        self.assertEqual(result["type"], "neev.steer")
        self.assertEqual(result["requestId"], "req-123")
        self.assertTrue(result["accepted"])

    def test_steer_rejected(self):
        output = io.StringIO()
        emit = adapter.Emitter(output)
        emit.steer_rejected("req-123", "run finished")
        result = json.loads(output.getvalue().strip())
        self.assertEqual(result["type"], "neev.steer")
        self.assertEqual(result["requestId"], "req-123")
        self.assertFalse(result["accepted"])
        self.assertEqual(result["reason"], "run finished")


class HandleEventTests(unittest.TestCase):
    def setUp(self):
        self.output = io.StringIO()
        self.emit = adapter.Emitter(self.output)
        self.state = adapter.SteerState(self.emit)

    def test_system_init_marks_session_started(self):
        event = {"type": "system", "subtype": "init", "session_id": "s1"}
        line = json.dumps(event)
        result = adapter.handle_event(event, line, self.emit, self.state)
        self.assertIsNone(result)
        self.assertTrue(self.state.is_session_started())
        # Event passed through
        self.assertIn("system", self.output.getvalue())

    def test_result_success_returns_zero(self):
        event = {"type": "result", "is_error": False, "subtype": "success"}
        line = json.dumps(event)
        result = adapter.handle_event(event, line, self.emit, self.state)
        self.assertEqual(result, 0)

    def test_result_is_error_returns_four(self):
        event = {"type": "result", "is_error": True, "subtype": "error_during_execution"}
        line = json.dumps(event)
        result = adapter.handle_event(event, line, self.emit, self.state)
        self.assertEqual(result, 4)

    def test_result_error_subtype_returns_four(self):
        event = {"type": "result", "is_error": False, "subtype": "error_max_turns"}
        line = json.dumps(event)
        result = adapter.handle_event(event, line, self.emit, self.state)
        self.assertEqual(result, 4)

    def test_result_suppressed_when_interrupted(self):
        # Register steer increments suppress count
        self.state.register_steer("uuid-1", "req-1")
        event = {"type": "result", "is_error": False, "subtype": "success"}
        line = json.dumps(event)
        result = adapter.handle_event(event, line, self.emit, self.state)
        # Suppressed, returns None
        self.assertIsNone(result)
        # Not passed through
        self.assertNotIn("result", self.output.getvalue())

    def test_user_replay_confirms_steer(self):
        self.state.register_steer("uuid-1", "req-1")
        event = {"type": "user", "uuid": "uuid-1", "isReplay": True}
        line = json.dumps(event)
        result = adapter.handle_event(event, line, self.emit, self.state)
        self.assertIsNone(result)
        # Check steer accepted
        lines = [json.loads(l) for l in self.output.getvalue().strip().split("\n") if l]
        # Find neev.steer
        steer_msgs = [l for l in lines if l.get("type") == "neev.steer"]
        self.assertEqual(len(steer_msgs), 1)
        self.assertTrue(steer_msgs[0]["accepted"])

    def test_control_response_handled_internally(self):
        control_id = self.state.register_steer("uuid-1", "req-1")
        # SDK wire format: {type:"control_response", response:{request_id, subtype:"success", response:{still_queued:[]}}}
        event = {
            "type": "control_response",
            "response": {
                "request_id": control_id,
                "subtype": "success",
                "response": {"still_queued": ["uuid-1"]}
            }
        }
        line = json.dumps(event)
        result = adapter.handle_event(event, line, self.emit, self.state)
        self.assertIsNone(result)
        # Control response NOT passed through
        output_lines = [l for l in self.output.getvalue().strip().split("\n") if l]
        for ol in output_lines:
            parsed = json.loads(ol)
            self.assertNotEqual(parsed.get("type"), "control_response")

    def test_other_events_passed_through(self):
        event = {"type": "stream_event", "event": {"type": "message_start"}}
        line = json.dumps(event)
        result = adapter.handle_event(event, line, self.emit, self.state)
        self.assertIsNone(result)
        self.assertIn("stream_event", self.output.getvalue())


class EndToEndTests(unittest.TestCase):
    """Full adapter tests against fake Claude CLI."""

    def run_adapter(self, mode, extra=()):
        with tempfile.TemporaryDirectory() as directory:
            environment = dict(os.environ, FAKE_MODE=mode)
            process = subprocess.run(
                [sys.executable, ADAPTER, "--claude", fake_claude(directory),
                 "--cwd", directory, *extra, "--", "hello"],
                capture_output=True, text=True, timeout=30, env=environment,
            )
            lines = []
            for line in process.stdout.splitlines():
                if line.strip():
                    try:
                        lines.append(json.loads(line))
                    except json.JSONDecodeError:
                        pass
            return process.returncode, lines

    def test_a_claude_that_dies_early_is_explained_by_its_own_stderr(self):
        code, lines = self.run_adapter("dies")
        self.assertEqual(code, 4)
        self.assertEqual(lines[-1], {
            "type": "turn.failed",
            "reason": "provider exited: Invalid API key \u00b7 Please run /login",
        })

    def test_turn_streams_and_completes(self):
        code, lines = self.run_adapter("ok")
        self.assertEqual(code, 0)
        # Check for native Claude events passed through
        types = [entry.get("type") for entry in lines]
        self.assertIn("system", types)
        self.assertIn("result", types)

    def test_tool_events_passed_through(self):
        code, lines = self.run_adapter("with_tool")
        self.assertEqual(code, 0)
        # Check for stream_event passed through
        stream_events = [l for l in lines if l.get("type") == "stream_event"]
        self.assertGreater(len(stream_events), 0)

    def test_error_result_exits_nonzero(self):
        code, lines = self.run_adapter("immediate_error")
        self.assertEqual(code, 4)
        result = next((l for l in lines if l.get("type") == "result"), None)
        self.assertIsNotNone(result)
        self.assertTrue(result.get("is_error"))

    def test_provider_did_not_start(self):
        process = subprocess.run(
            [sys.executable, ADAPTER, "--claude", "/nonexistent/claude", "--", "hi"],
            capture_output=True, text=True, timeout=30,
        )
        self.assertEqual(process.returncode, 3)
        result = json.loads(process.stdout.strip().split("\n")[0])
        self.assertEqual(result["type"], "turn.failed")

    def test_empty_prompt_rejected(self):
        process = subprocess.run(
            [sys.executable, ADAPTER, "--claude", "/nonexistent/claude", "--", ""],
            capture_output=True, text=True, timeout=30,
        )
        self.assertEqual(process.returncode, 2)

    def test_flags_preserved(self):
        """Verify launcher flags are passed to Claude CLI."""
        with tempfile.TemporaryDirectory() as directory:
            args = adapter._parse([
                "--claude", fake_claude(directory),
                "--model", "opus",
                "--effort", "high",
                "--verbose",
                "--include-partial-messages",
                "--append-system-prompt", "Be helpful",
                "--settings", "/path/settings.json",
                "--", "hi"
            ])
            argv = adapter.claude_argv(args)
            self.assertIn("--model", argv)
            self.assertIn("opus", argv)
            self.assertIn("--effort", argv)
            self.assertIn("high", argv)
            self.assertIn("--verbose", argv)
            self.assertIn("--include-partial-messages", argv)
            self.assertIn("--append-system-prompt", argv)
            self.assertIn("Be helpful", argv)
            self.assertIn("--settings", argv)
            self.assertIn("/path/settings.json", argv)


class SteeringEndToEndTests(unittest.TestCase):
    """End-to-end tests for interrupt-and-continue steering."""

    def test_steer_accepted_via_control_response_and_echo(self):
        """Full steering flow: control_request -> control_response -> user replay."""
        output = io.StringIO()
        emit = adapter.Emitter(output)
        state = adapter.SteerState(emit)

        # 1. Register steer
        uuid_val = "test-uuid"
        request_id = "req-1"
        control_id = state.register_steer(uuid_val, request_id)
        self.assertIsNotNone(control_id)

        # 2. Receive control_response with uuid in still_queued
        # SDK wire format: {type:"control_response", response:{request_id, subtype:"success", response:{still_queued:[]}}}
        response = {
            "type": "control_response",
            "response": {
                "request_id": control_id,
                "subtype": "success",
                "response": {"still_queued": [uuid_val]}
            }
        }
        state.handle_control_response(response)

        # 3. Receive user replay
        state.confirm_echo(uuid_val)

        # Should have emitted acceptance
        lines = [json.loads(l) for l in output.getvalue().strip().split("\n") if l]
        steer_msgs = [l for l in lines if l.get("type") == "neev.steer"]
        self.assertEqual(len(steer_msgs), 1)
        self.assertTrue(steer_msgs[0]["accepted"])
        self.assertEqual(steer_msgs[0]["requestId"], request_id)

    def test_steer_rejected_when_not_queued(self):
        """Steer rejected when UUID not in still_queued."""
        output = io.StringIO()
        emit = adapter.Emitter(output)
        state = adapter.SteerState(emit)

        control_id = state.register_steer("uuid-1", "req-1")
        # SDK wire format
        response = {
            "type": "control_response",
            "response": {
                "request_id": control_id,
                "subtype": "success",
                "response": {"still_queued": []}
            }
        }
        state.handle_control_response(response)

        lines = [json.loads(l) for l in output.getvalue().strip().split("\n") if l]
        self.assertEqual(len(lines), 1)
        self.assertFalse(lines[0]["accepted"])
        self.assertEqual(lines[0]["reason"], "message not queued")

    def test_echo_before_control_response(self):
        """Steer accepted even if echo arrives before control_response."""
        output = io.StringIO()
        emit = adapter.Emitter(output)
        state = adapter.SteerState(emit)

        state.register_steer("uuid-1", "req-1")
        # Echo arrives first
        state.confirm_echo("uuid-1")

        lines = [json.loads(l) for l in output.getvalue().strip().split("\n") if l]
        self.assertEqual(len(lines), 1)
        self.assertTrue(lines[0]["accepted"])

    def test_finished_run_rejects_steer(self):
        """A completed run must reject steering."""
        output = io.StringIO()
        emit = adapter.Emitter(output)
        state = adapter.SteerState(emit)

        state.mark_finished()
        control_id = state.register_steer("uuid-1", "req-1")
        self.assertIsNone(control_id)

    def test_intermediate_result_suppressed(self):
        """Result from interrupted turn is suppressed."""
        output = io.StringIO()
        emit = adapter.Emitter(output)
        state = adapter.SteerState(emit)

        # Register steer increments suppress count
        state.register_steer("uuid-1", "req-1")
        self.assertTrue(state.should_suppress_result())
        # Second result not suppressed
        self.assertFalse(state.should_suppress_result())


if __name__ == "__main__":
    unittest.main()
