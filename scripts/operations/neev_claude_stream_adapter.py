#!/usr/bin/env python3
"""Run Claude turns through stream-json with interrupt-and-continue steering.

Unlike Codex (`turn/steer` RPC within one turn), Claude uses queued user messages
plus interrupt to continue within the SAME CLI process and Neev run. The adapter:

1. Starts Claude with `-p "" --input-format stream-json --output-format stream-json`
2. Sends the initial prompt as a user message via stdin (keeping -p for print mode)
3. Passes through native Claude events unchanged to stdout
4. On steering input {requestId, text}:
   - Queues a follow-up user message with unique UUID
   - Sends control_request with interrupt (subtype not type)
   - Verifies UUID in control_response.response.response.still_queued
   - Waits for provider replay echo as acceptance
   - Emits ONLY {type:"neev.steer", requestId, accepted} on confirmation

SDK wire format (from claude-agent-sdk-python):
  control_request: {type:"control_request", request_id, request:{subtype:"interrupt"}}
  control_response: {type:"control_response", response:{request_id, subtype:"success", response:{still_queued:[]}}}

Bounded everywhere. Same sandbox/egress, no control sockets. Preserves context
and tool state across interrupt-and-continue. A completed run rejects steering.
"""

import argparse
import json
import os
import signal
import subprocess
import sys
import threading
import time
import uuid as uuid_module

# Line bounds matching Codex adapter
MAX_LINE_BYTES = 64 * 1024 * 1024

# Handshake timeout (first streaming output)
HANDSHAKE_TIMEOUT_SECS = 60

# Steering input bounds
STEER_MAX_LINE_BYTES = 64 * 1024
STEER_MAX_TEXT_BYTES = 8 * 1024

# Maximum pending steers to prevent unbounded memory
MAX_PENDING_STEERS = 16

# Interrupt timeout - reject steer if no response within this time
INTERRUPT_TIMEOUT_SECS = 30


def _parse(argv):
    parser = argparse.ArgumentParser(prog="neev_claude_stream_adapter", add_help=True)
    parser.add_argument("--claude", required=True, help="Path to claude CLI binary")
    parser.add_argument("--cwd", default=None)
    parser.add_argument("--model", default=None)
    parser.add_argument("--effort", default=None)
    parser.add_argument(
        "--append-system-prompt", default=None, dest="append_system_prompt"
    )
    parser.add_argument("--settings", default=None)
    # Flag options preserved from launcher
    parser.add_argument("--verbose", action="store_true", default=False)
    parser.add_argument(
        "--include-partial-messages", action="store_true", default=False,
        dest="include_partial_messages"
    )
    parser.add_argument(
        "--enable-steering", action="store_true", default=False, dest="enable_steering"
    )
    parser.add_argument("prompt", nargs=argparse.REMAINDER)
    args = parser.parse_args(argv)
    prompt = list(args.prompt)
    if prompt and prompt[0] == "--":
        prompt = prompt[1:]
    args.prompt = prompt[0] if prompt else ""
    return args


def claude_argv(args):
    """Build Claude CLI argv for persistent stream-json mode.

    Keep -p with empty string to activate print mode, prompt sent via stdin.
    All launcher flags are preserved.
    """
    argv = [args.claude]
    # -p "" activates print mode; actual prompt comes via stdin
    argv += ["-p", ""]
    # Stream JSON input/output for steering
    argv += ["--output-format", "stream-json"]
    argv += ["--input-format", "stream-json"]
    # Echo user messages back for steering acknowledgment
    argv += ["--replay-user-messages"]
    # --verbose is REQUIRED when using -p "" with stream-json output
    argv.append("--verbose")
    if args.include_partial_messages:
        argv.append("--include-partial-messages")
    # Value options
    if args.model:
        argv += ["--model", args.model]
    if args.effort:
        argv += ["--effort", args.effort]
    if args.append_system_prompt:
        argv += ["--append-system-prompt", args.append_system_prompt]
    if args.settings:
        argv += ["--settings", args.settings]
    return argv


class Emitter:
    """Passes through native Claude events, emits only neev.steer for steering."""

    def __init__(self, out, lock=None):
        self._out = out
        self._lock = lock or threading.Lock()

    def _write(self, payload):
        with self._lock:
            self._out.write(json.dumps(payload, ensure_ascii=False) + "\n")
            self._out.flush()

    def passthrough(self, line):
        """Pass through native Claude event line unchanged."""
        with self._lock:
            self._out.write(line)
            if not line.endswith("\n"):
                self._out.write("\n")
            self._out.flush()

    def steer_accepted(self, request_id):
        """Acknowledge steering input accepted by provider."""
        self._write({
            "type": "neev.steer",
            "requestId": request_id,
            "accepted": True,
        })

    def steer_rejected(self, request_id, reason):
        """Report steering input rejected."""
        self._write({
            "type": "neev.steer",
            "requestId": request_id,
            "accepted": False,
            "reason": reason,
        })

    def turn_failed(self, reason):
        """Emit turn failed in native envelope."""
        self._write({"type": "turn.failed", "reason": reason})


class SteerState:
    """Thread-safe state for active steering within one Claude process.

    Tracks pending steer requests, interrupt state, and continuation.
    Bounded: max MAX_PENDING_STEERS entries.
    """

    def __init__(self, emit):
        self._lock = threading.Lock()
        self._emit = emit
        self._finished = False
        self._session_started = False
        # {uuid: request_id} for pending steers awaiting echo
        self._pending_steers = {}
        # {control_request_id: (uuid, timestamp)} for pending interrupt requests
        self._pending_interrupts = {}
        # UUIDs confirmed in still_queued but not yet echoed
        self._confirmed_queued = set()
        # Next control request ID
        self._next_control_id = 1
        # True while waiting for continuation after interrupt
        self._interrupt_pending = False
        # Count of pending result messages to suppress
        self._suppress_results = 0

    def mark_session_started(self):
        with self._lock:
            self._session_started = True

    def is_session_started(self):
        with self._lock:
            return self._session_started

    def mark_finished(self):
        with self._lock:
            self._finished = True
            # Reject any pending steers
            for uuid_val, request_id in list(self._pending_steers.items()):
                self._emit.steer_rejected(request_id, "run finished")
            self._pending_steers.clear()
            self._pending_interrupts.clear()
            self._confirmed_queued.clear()

    def is_finished(self):
        with self._lock:
            return self._finished

    def should_suppress_result(self):
        """Check if current result should be suppressed (intermediate)."""
        with self._lock:
            if self._suppress_results > 0:
                self._suppress_results -= 1
                return True
            return False

    def register_steer(self, uuid_val, request_id):
        """Register a pending steer awaiting provider echo. Returns control_id or None."""
        with self._lock:
            if self._finished:
                return None
            if len(self._pending_steers) >= MAX_PENDING_STEERS:
                return None
            self._pending_steers[uuid_val] = request_id
            control_id = f"neev-ctrl-{self._next_control_id}"
            self._next_control_id += 1
            # Store with timestamp for timeout tracking
            self._pending_interrupts[control_id] = (uuid_val, time.monotonic())
            self._interrupt_pending = True
            self._suppress_results += 1  # Suppress next result (interrupted)
            return control_id

    def check_interrupt_timeouts(self):
        """Check for timed-out interrupt requests. Called periodically."""
        with self._lock:
            now = time.monotonic()
            timed_out = []
            for ctrl_id, (uuid_val, ts) in list(self._pending_interrupts.items()):
                if now - ts > INTERRUPT_TIMEOUT_SECS:
                    timed_out.append((ctrl_id, uuid_val))

            for ctrl_id, uuid_val in timed_out:
                del self._pending_interrupts[ctrl_id]
                ext_request_id = self._pending_steers.pop(uuid_val, None)
                if ext_request_id:
                    self._emit.steer_rejected(ext_request_id, "interrupt timeout")
                    self._suppress_results = max(0, self._suppress_results - 1)
                self._confirmed_queued.discard(uuid_val)

            if timed_out and not self._pending_interrupts:
                self._interrupt_pending = False

    def handle_control_response(self, message):
        """Process control_response from provider. Returns True if handled.

        SDK wire format (from claude-agent-sdk-python):
          {type:"control_response", response:{request_id, subtype:"success"|"error", response:{still_queued:[]}, error:"str"}}
        Note: request_id is inside response, not top-level. Uses 'subtype' not 'status'.
        """
        with self._lock:
            # Extract nested response object
            resp = message.get("response", {})
            request_id = resp.get("request_id")
            entry = self._pending_interrupts.pop(request_id, None)
            if entry is None:
                return False
            uuid_val, _timestamp = entry

            subtype = resp.get("subtype")
            if subtype == "success":
                # Inner response contains still_queued
                inner_resp = resp.get("response", {})
                still_queued = inner_resp.get("still_queued", [])
                if uuid_val in still_queued:
                    # UUID is queued, wait for echo
                    self._confirmed_queued.add(uuid_val)
                else:
                    # UUID not queued, reject and undo suppression
                    ext_request_id = self._pending_steers.pop(uuid_val, None)
                    if ext_request_id:
                        self._emit.steer_rejected(ext_request_id, "message not queued")
                        self._suppress_results = max(0, self._suppress_results - 1)
                    self._interrupt_pending = False
            else:
                # Interrupt failed (subtype == "error")
                error = resp.get("error", "interrupt failed")
                ext_request_id = self._pending_steers.pop(uuid_val, None)
                if ext_request_id:
                    self._emit.steer_rejected(ext_request_id, str(error)[:100])
                    self._suppress_results = max(0, self._suppress_results - 1)
                self._interrupt_pending = False
            return True

    def confirm_echo(self, uuid_val):
        """Provider echoed user message (replay). Returns True if steer accepted."""
        with self._lock:
            # Accept if uuid was confirmed queued OR if we're still waiting
            # (echo can arrive before control_response)
            request_id = self._pending_steers.pop(uuid_val, None)
            self._confirmed_queued.discard(uuid_val)
            if request_id:
                self._emit.steer_accepted(request_id)
                self._interrupt_pending = False
                return True
            return False

    def get_pending_count(self):
        with self._lock:
            return len(self._pending_steers)


def _parse_steer_input(line):
    """Parse steering input {requestId, text}. Returns (request_id, text) or raises."""
    if len(line) > STEER_MAX_LINE_BYTES:
        raise ValueError("oversized")
    record = json.loads(line)
    if not isinstance(record, dict):
        raise ValueError("not a record")
    request_id = record.get("requestId")
    text = record.get("text")
    if not isinstance(request_id, str) or not request_id:
        raise ValueError("missing requestId")
    if not isinstance(text, str):
        raise ValueError("missing text")
    if len(text.encode("utf-8")) > STEER_MAX_TEXT_BYTES:
        raise ValueError("text too large")
    return request_id, text


def _build_user_message(text, uuid_val):
    """Build SDK user message envelope for stream-json input."""
    return {
        "type": "user",
        "uuid": uuid_val,
        "message": {
            "role": "user",
            "content": text
        },
        "parent_tool_use_id": None
    }


def _build_control_request(control_id):
    """Build SDK control_request for interrupt.

    SDK wire format (from claude-agent-sdk-python _send_control_request):
      {type:"control_request", request_id:"id", request:{subtype:"interrupt"}}
    Note: uses 'subtype' not 'type' inside request.
    """
    return {
        "type": "control_request",
        "request_id": control_id,
        "request": {
            "subtype": "interrupt"
            # Never use cancel_queued - we want our message to stay queued
        }
    }


def _send_json(stdin, obj):
    """Send JSON object to stdin. Raises on pipe error."""
    stdin.write(json.dumps(obj) + "\n")
    stdin.flush()


def _read_line_bounded(stream, max_bytes):
    """Read one line with bound. Returns line or None at EOF. Raises on oversized."""
    line = stream.readline(max_bytes + 1)
    if not line:
        return None
    if len(line) > max_bytes:
        raise ValueError("oversized")
    return line


def _steer_reader(steer_input, provider_stdin, state, emit):
    """Background thread: read steering commands, queue + interrupt.

    Bounded line reads. For each {requestId, text}:
    1. Generate UUID for the follow-up message
    2. Register pending steer (bounded)
    3. Send user message to Claude stdin
    4. Send control_request interrupt
    5. Main loop handles control_response and echo
    """
    lines_read = 0
    max_lines = 1000  # Bound total steering inputs per session

    try:
        while lines_read < max_lines:
            line = steer_input.readline(STEER_MAX_LINE_BYTES + 1)
            if not line:
                break
            lines_read += 1
            line = line.strip()
            if not line:
                continue

            if state.is_finished():
                try:
                    request_id, _ = _parse_steer_input(line)
                    emit.steer_rejected(request_id, "run finished")
                except (ValueError, json.JSONDecodeError):
                    emit.steer_rejected("unknown", "run finished")
                continue

            try:
                request_id, text = _parse_steer_input(line)
            except (ValueError, json.JSONDecodeError) as error:
                emit.steer_rejected("unknown", str(error)[:100])
                continue

            # Check pending limit
            if state.get_pending_count() >= MAX_PENDING_STEERS:
                emit.steer_rejected(request_id, "too many pending steers")
                continue

            # Generate UUID for this follow-up
            uuid_val = str(uuid_module.uuid4())

            # Register and get control ID
            control_id = state.register_steer(uuid_val, request_id)
            if control_id is None:
                emit.steer_rejected(request_id, "registration failed")
                continue

            try:
                # Queue the follow-up BEFORE interrupt
                _send_json(provider_stdin, _build_user_message(text, uuid_val))
                # Now send interrupt control request
                _send_json(provider_stdin, _build_control_request(control_id))
            except (BrokenPipeError, OSError):
                emit.steer_rejected(request_id, "provider closed")
                break

    except (OSError, ValueError):
        pass  # Input closed; main loop handles outcome


def handle_event(event, line, emit, state):
    """Handle one Claude event. Returns exit code or None to continue.

    Passes through most events unchanged. Only intercepts:
    - system/init: marks session started
    - result: checks is_error, may suppress if interrupted
    - user (replay): confirms steering
    - control_response: processes interrupt receipt
    """
    event_type = event.get("type")

    # System init message
    if event_type == "system":
        subtype = event.get("subtype")
        if subtype == "init":
            state.mark_session_started()
        emit.passthrough(line)
        return None

    # Result message = run completed
    if event_type == "result":
        # Check if this is an intermediate result to suppress
        if state.should_suppress_result():
            # Don't pass through interrupted result
            return None
        # Check for error
        is_error = event.get("is_error", False)
        subtype = event.get("subtype", "")
        emit.passthrough(line)
        if is_error or subtype.startswith("error"):
            return 4
        return 0

    # User message replay = steering acknowledgment
    if event_type == "user":
        # Check if this is a replay (has isReplay field or session_id required)
        uuid_val = event.get("uuid")
        is_replay = event.get("isReplay", False)
        # Even without isReplay, if uuid matches pending, it's our steer
        if uuid_val and state.confirm_echo(uuid_val):
            # Steer accepted, emit our ack (already done in confirm_echo)
            pass
        # Pass through user replays
        emit.passthrough(line)
        return None

    # Control response = interrupt receipt
    if event_type == "control_response":
        if state.handle_control_response(event):
            # Handled internally, don't pass through
            return None
        # Unknown control response, pass through
        emit.passthrough(line)
        return None

    # All other events pass through unchanged
    emit.passthrough(line)
    return None


class HandshakeTimeout(Exception):
    """Provider did not produce output in time."""


def _start_handshake_deadline(seconds=None):
    seconds = HANDSHAKE_TIMEOUT_SECS if seconds is None else seconds

    def expire(*_unused):
        raise HandshakeTimeout()

    signal.signal(signal.SIGALRM, expire)
    signal.alarm(seconds)


def _clear_handshake_deadline():
    signal.alarm(0)


def drive(child, args, emit, steer_input=None):
    """Run Claude with stream-json, handling steering via interrupt."""
    stdin, stdout = child.stdin, child.stdout
    state = SteerState(emit)
    steer_thread = None

    # Send initial prompt as user message
    initial_uuid = str(uuid_module.uuid4())
    try:
        _send_json(stdin, _build_user_message(args.prompt, initial_uuid))
    except (BrokenPipeError, OSError):
        emit.turn_failed("failed to send prompt")
        return 3

    _start_handshake_deadline()

    # Start steering reader if enabled
    if steer_input is not None:
        steer_thread = threading.Thread(
            target=_steer_reader,
            args=(steer_input, stdin, state, emit),
            daemon=True,
        )
        steer_thread.start()

    try:
        while True:
            line = _read_line_bounded(stdout, MAX_LINE_BYTES)
            if line is None:
                state.mark_finished()
                if not state.is_session_started():
                    emit.turn_failed("provider closed before session started")
                else:
                    emit.turn_failed("provider closed the stream")
                return 4

            try:
                event = json.loads(line)
            except ValueError:
                # Non-JSON line, pass through
                emit.passthrough(line)
                continue

            if not isinstance(event, dict):
                emit.passthrough(line)
                continue

            # Clear handshake deadline on first valid event
            if not state.is_session_started():
                if event.get("type") == "system" and event.get("subtype") == "init":
                    _clear_handshake_deadline()

            done = handle_event(event, line, emit, state)
            if done is not None:
                state.mark_finished()
                return done

    except HandshakeTimeout:
        state.mark_finished()
        emit.turn_failed("provider did not start the session")
        return 4
    except ValueError as e:
        state.mark_finished()
        emit.turn_failed(f"oversized provider message: {e}")
        return 4
    except OSError:
        state.mark_finished()
        emit.turn_failed("provider stream ended")
        return 4


def main(argv=None, steer_input=None):
    """Run one Claude session with optional steering."""
    args = _parse(sys.argv[1:] if argv is None else argv)
    cwd = args.cwd or os.getcwd()
    emit = Emitter(sys.stdout)

    if not args.prompt:
        emit.turn_failed("empty prompt")
        return 2

    # Determine steering input
    if steer_input is not None:
        effective_steer = steer_input
    elif args.enable_steering:
        effective_steer = sys.stdin
    else:
        effective_steer = None

    try:
        child = subprocess.Popen(
            claude_argv(args),
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            cwd=cwd,
            text=True,
        )
    except OSError:
        emit.turn_failed("provider did not start")
        return 3

    try:
        return drive(child, args, emit, steer_input=effective_steer)
    finally:
        _clear_handshake_deadline()
        for pipe in (child.stdin, child.stdout):
            try:
                pipe.close()
            except (OSError, ValueError):
                pass
        child.terminate()
        try:
            child.wait(timeout=10)
        except subprocess.TimeoutExpired:
            child.kill()
            child.wait()


if __name__ == "__main__":
    sys.exit(main())
