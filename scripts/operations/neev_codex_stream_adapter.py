#!/usr/bin/env python3
"""Run one Codex turn through `codex app-server` and stream it as it is written.

`codex exec --json` has no token-level output: the reply arrives whole, after the
turn is already over. The app-server does emit `item/agentMessage/delta`, but
driving it needs a writable stdin — and the launcher gives every paid turn
`stdin=DEVNULL` on purpose, so nothing outside the sandbox can feed a prompt or a
provider stream in.

So this runs INSIDE the sandbox, as the provider process itself. It owns the
app-server's stdin and stdout as an ordinary child pipe, and writes the turn to
ITS stdout in the same `item.*` envelopes `codex exec --json` produces, plus
`item.delta` for the fragments. The launcher forwards those lines unchanged and
the Rust parser reads one vocabulary for both transports. Nothing crosses the
sandbox boundary that did not cross it before: the prompt still arrives as argv,
the outer stdin stays closed.

Bounded everywhere. Lines from the app-server are read with a cap, oversized ones
end the turn rather than being half-read, and nothing accumulates per turn except
a small set of open item ids.

Failures are reported as `turn.failed` on stdout, never as a silent exit: a turn
that produced nothing must say why.
"""

import argparse
import json
import os
import select
import signal
import subprocess
import sys
import threading
from collections import deque

# App-server command completions repeat the full output, even after chunked
# deltas. Allow large HTML results on input, then bound the displayed preview.
MAX_LINE_BYTES = 64 * 1024 * 1024
MAX_TOOL_PREVIEW_CHARS = 4096
# How long the handshake may take before the turn is abandoned. It bounds ONLY
# `initialize` → `thread/start` → `turn/start`, never the turn itself, which
# legitimately takes minutes. Without it a provider that answers nothing is a
# run that produces no output and no reason, and looks from the outside exactly
# like a run that did nothing.
HANDSHAKE_TIMEOUT_SECS = 60

# Our sandbox IS the boundary, so Codex's own must be off: it ships bubblewrap
# and cannot build a sandbox inside ours (empty CapBnd => bwrap dies), which
# reached readers as "the shell tool failed". `exec` spells this
# `--dangerously-bypass-approvals-and-sandbox`; the app-server spells it as
# thread parameters. Approvals go with it because nobody can answer a prompt.
SANDBOX_MODE = "danger-full-access"
APPROVAL_POLICY = "never"

# How much of a provider's failure text is forwarded. The runtime redacts and
# cuts it again; this bound only keeps one pathological message from being huge.
MAX_REASON_CHARS = 1000
# The provider's stderr is kept as a short tail, only to explain a failure that
# happened before the protocol could (a config error, a missing sign-in).
STDERR_TAIL_LINES = 8
STDERR_LINE_CHARS = 500

# Usage evidence for Auto routing: Codex's documented `account/rateLimits`
# data, reduced to windows only (never credits, plan or names) and bounded.
RATE_LIMITS_READ_ID = 4
MAX_USAGE_PAYLOAD_BYTES = 8 * 1024
MAX_USAGE_POOLS = 8
_WINDOW_FIELDS = ("usedPercent", "windowDurationMins", "resetsAt")


def _window(value):
    if not isinstance(value, dict):
        return None
    out = {}
    for field in _WINDOW_FIELDS:
        item = value.get(field)
        if item is None or (isinstance(item, (int, float)) and not isinstance(item, bool)):
            out[field] = item
    return out if "usedPercent" in out and out["usedPercent"] is not None else None


def _snapshot(value):
    if not isinstance(value, dict):
        return None
    out = {}
    limit_id = value.get("limitId")
    if isinstance(limit_id, str) and len(limit_id) <= 64:
        out["limitId"] = limit_id
    for key in ("primary", "secondary"):
        window = _window(value.get(key))
        if window is not None:
            out[key] = window
    return out if ("primary" in out or "secondary" in out) else None


def reduce_rate_limits(params):
    """Keep only the documented windows of a rate-limit report, or None."""
    if not isinstance(params, dict):
        return None
    payload = {}
    single = _snapshot(params.get("rateLimits"))
    if single is not None:
        payload["rateLimits"] = single
    pools = params.get("rateLimitsByLimitId")
    if isinstance(pools, dict):
        kept = {}
        for key, value in list(pools.items())[:MAX_USAGE_POOLS]:
            snapshot = _snapshot(value)
            if isinstance(key, str) and len(key) <= 64 and snapshot is not None:
                kept[key] = snapshot
        if kept:
            payload["rateLimitsByLimitId"] = kept
    if not payload:
        return None
    if len(json.dumps(payload)) > MAX_USAGE_PAYLOAD_BYTES:
        return None
    return payload


def error_hint(error):
    """Pure: the kind of a v2 `TurnError.codexErrorInfo`, or None.

    It is either a plain string (`"usageLimitExceeded"`, `"unauthorized"`) or a
    one-key object carrying details (`{"httpConnectionFailed": {...}}`).
    """
    info = (error or {}).get("codexErrorInfo")
    if isinstance(info, str):
        return info
    if isinstance(info, dict) and len(info) == 1:
        return next(iter(info))
    return None


def error_text(error, fallback):
    """Pure: the provider's message for a TurnError, with its details."""
    error = error or {}
    message = error.get("message") or fallback
    details = error.get("additionalDetails")
    if isinstance(details, str) and details.strip() and details not in message:
        message = f"{message} — {details}"
    return str(message)


class StderrTail:
    """Drain the provider's stderr on a thread, keeping only the last lines.

    Draining matters on its own: a provider that fills an unread stderr pipe
    blocks. Kept lines are bounded in count and length, and are used for ONE
    thing — saying why a provider that died before the protocol could explain
    itself did so.
    """

    def __init__(self, stream):
        self._lines = deque(maxlen=STDERR_TAIL_LINES)
        self._lock = threading.Lock()
        self._thread = None
        if stream is not None:
            self._thread = threading.Thread(target=self._drain, args=(stream,), daemon=True)
            self._thread.start()

    def _drain(self, stream):
        try:
            for line in stream:
                text = line.strip()
                if text:
                    with self._lock:
                        self._lines.append(text[:STDERR_LINE_CHARS])
        except (OSError, ValueError):
            pass

    def last(self, wait=0.5):
        """The last meaningful line, after giving the drain a moment to finish."""
        if self._thread is not None:
            self._thread.join(wait)
        with self._lock:
            for line in reversed(self._lines):
                # Log-framework noise ("WARN ..." about PATH aliases) is not a reason.
                if "could not create PATH aliases" in line:
                    continue
                return line
        return None


def _exited(stderr_tail, fallback, code):
    """(reason, code) for a provider that went away: its own last words if any."""
    tail = stderr_tail.last() if stderr_tail is not None else None
    if tail:
        # No hint: the words say more than "closed" does (e.g. "not logged in").
        return f"provider exited: {tail}", None
    return fallback, code


# Bounded stdin relay for steering. The runtime sends JSON records with
# {requestId, text} to add follow-up input to the active turn.
STEER_MAX_LINE_BYTES = 64 * 1024
STEER_MAX_TEXT_BYTES = 8 * 1024


def _parse(argv):
    parser = argparse.ArgumentParser(prog="neev_codex_stream_adapter", add_help=True)
    parser.add_argument("--codex", required=True)
    # Defaults to the process's own cwd, which the sandbox already set to the
    # run workdir; an explicit value is for tests.
    parser.add_argument("--cwd", default=None)
    parser.add_argument("--model", default=None)
    parser.add_argument("--effort", default=None)
    parser.add_argument("--service-tier", default=None, dest="service_tier")
    parser.add_argument("--config", action="append", default=[], dest="config")
    # The shared response-style policy, as the protocol's own developer
    # channel. Never the user's prompt, and never `baseInstructions`: that
    # would REPLACE Codex's own instructions instead of adding to them.
    parser.add_argument(
        "--developer-instructions", default=None, dest="developer_instructions"
    )
    parser.add_argument("--image", action="append", default=[], dest="image")
    # Enable mid-run steering: read JSON {requestId, text} from stdin and relay
    # as turn/steer. When enabled, stdin is the steering channel, not DEVNULL.
    parser.add_argument(
        "--enable-steering", action="store_true", default=False, dest="enable_steering"
    )
    parser.add_argument("prompt", nargs=argparse.REMAINDER)
    args = parser.parse_args(argv)
    prompt = list(args.prompt)
    if prompt and prompt[0] == "--":
        prompt = prompt[1:]
    # The prompt is one argument and is passed through untouched — the same
    # literal `codex exec` receives, including any leading dash.
    args.prompt = prompt[0] if prompt else ""
    return args


def codex_argv(args):
    """Pure: the app-server command line.

    Config overrides keep the EXACT spelling the exec path already sends in
    production (`service_tier="fast"`, `features.fast_mode=true`), because that
    is the form verified against the pinned binary. They are global options, so
    they precede the subcommand.
    """
    argv = [args.codex]
    for override in args.config:
        argv += ["-c", override]
    argv.append("app-server")
    return argv


def turn_input(prompt, images):
    """Pure: the turn's input items.

    Images are `localImage` inputs holding the path the runtime already staged
    inside this run's workdir — the same files `codex exec -i` was given.
    """
    items = [{"type": "text", "text": prompt}]
    for path in images:
        items.append({"type": "localImage", "path": path})
    return items


def thread_params(args, cwd):
    params = {
        "cwd": cwd,
        "sandbox": SANDBOX_MODE,
        "approvalPolicy": APPROVAL_POLICY,
    }
    if args.model:
        params["model"] = args.model
    if args.service_tier:
        params["serviceTier"] = args.service_tier
    # `developerInstructions` is a thread property in the pinned schema
    # (string | null), separate from `baseInstructions`, which we never send:
    # the provider's own base prompt stays, this is added alongside it.
    if args.developer_instructions:
        params["developerInstructions"] = args.developer_instructions
    return params


def start_params(args, thread_id, cwd):
    params = {
        "threadId": thread_id,
        "input": turn_input(args.prompt, args.image),
        "cwd": cwd,
    }
    if args.model:
        params["model"] = args.model
    if args.effort:
        params["effort"] = args.effort
    if args.service_tier:
        params["serviceTier"] = args.service_tier
    return params


class Emitter:
    """Writes the envelopes the Rust parser already reads, one per line.

    Every completion is emitted, including one whose text already went out as
    deltas. Dropping the repeat here as well would leave the parser holding that
    item id forever — it releases an id when the completion arrives — and after
    enough messages its bounded set would stop tracking new ones and start
    delivering replies twice. One owner for dedupe, and it is the parser.
    """

    def __init__(self, out):
        self._out = out

    def _write(self, payload):
        self._out.write(json.dumps(payload, ensure_ascii=False) + "\n")
        # Flushed per line: a delta held in a buffer is not a delta.
        self._out.flush()

    def delta(self, item_id, text):
        if not text:
            return
        self._write(
            {"type": "item.delta", "item": {"type": "agent_message", "id": item_id, "delta": text}}
        )

    def message(self, item_id, text):
        self._write(
            {"type": "item.completed", "item": {"type": "agent_message", "id": item_id, "text": text}}
        )

    def command_started(self, item_id, command):
        self._write(
            {
                "type": "item.started",
                "item": {"type": "command_execution", "id": item_id, "command": command},
            }
        )

    def command_completed(self, item_id, command, output, exit_code):
        if len(output) > MAX_TOOL_PREVIEW_CHARS:
            output = output[:MAX_TOOL_PREVIEW_CHARS] + "\n[Output preview truncated]"
        self._write(
            {
                "type": "item.completed",
                "item": {
                    "type": "command_execution",
                    "id": item_id,
                    "command": command,
                    "aggregated_output": output,
                    "exit_code": exit_code,
                },
            }
        )

    def turn_completed(self):
        self._write({"type": "turn.completed"})

    def usage(self, params):
        """Forward a rate-limit report, reduced; nothing when unusable."""
        payload = reduce_rate_limits(params)
        if payload is not None:
            self._write({"type": "neev.usage", "source": "codex.rateLimits", "payload": payload})

    def turn_failed(self, reason, code=None):
        """The turn is over and did not succeed.

        `reason` is the provider's OWN words where it gave any (bounded here; the
        runtime redacts and bounds again before anything is stored or shown).
        `code` is a structured hint — Codex's `codexErrorInfo` kind, or one of
        ours (`closed`, `timeout`, `not_started`, `oversized`,
        `settings_mismatch`) — and is omitted when we have none rather than
        guessed, so the runtime classifies the text instead.
        """
        payload = {"type": "turn.failed", "reason": str(reason)[:MAX_REASON_CHARS]}
        if code:
            payload["code"] = str(code)[:64]
        self._write(payload)

    def steer_accepted(self, request_id, turn_id):
        """Acknowledge a steering input was accepted by the provider."""
        self._write({
            "type": "neev.steer",
            "requestId": request_id,
            "accepted": True,
            "turnId": turn_id,
        })

    def steer_rejected(self, request_id, reason):
        """Report a steering input was rejected."""
        self._write({
            "type": "neev.steer",
            "requestId": request_id,
            "accepted": False,
            "reason": reason,
        })


def translate(method, params, emit, memory=None):
    """One app-server notification -> zero or more of our envelopes.

    Names and fields are the version-matched schema's
    (`item/agentMessage/delta` carries `itemId`/`delta`; thread items are
    camelCase `agentMessage`/`commandExecution`). Anything else is ignored
    rather than guessed at — including reasoning, which is never surfaced.

    Returns `None` while the turn continues, or its exit code once it is over.
    """
    if method == "item/agentMessage/delta":
        emit.delta(params.get("itemId", ""), params.get("delta", ""))
        return None
    item = params.get("item") or {}
    kind = item.get("type")
    if method == "item/started":
        if kind == "commandExecution":
            emit.command_started(item.get("id", ""), item.get("command") or "")
        return None
    if method == "item/completed":
        if kind == "agentMessage":
            emit.message(item.get("id", ""), item.get("text") or "")
        elif kind == "commandExecution":
            emit.command_completed(
                item.get("id", ""),
                item.get("command") or "",
                item.get("aggregatedOutput") or "",
                item.get("exitCode"),
            )
        return None
    if method == "account/rateLimits/updated":
        emit.usage(params)
        return None
    if method == "error":
        # An error notification (possibly a retry in progress). Remembered, so a
        # failed turn that carries no error of its own can still say why.
        if memory is not None and isinstance(params.get("error"), dict):
            memory["error"] = params["error"]
        return None
    if method == "turn/completed":
        turn = params.get("turn") or {}
        status = turn.get("status")
        if status == "completed":
            emit.turn_completed()
            return 0
        # `interrupted` and `failed` both mean the reply is not finished. The
        # EXIT CODE is what settles the turn upstream, so a partial reply must
        # never leave here as a success.
        error = turn.get("error") or (memory or {}).get("error") or {}
        emit.turn_failed(error_text(error, status or "failed"), error_hint(error))
        return 4
    return None


def _read_line(stream):
    """One line, or None at EOF. Raises on an oversized message."""
    line = stream.readline(MAX_LINE_BYTES + 1)
    if not line:
        return None
    if len(line) > MAX_LINE_BYTES:
        raise ValueError("oversized")
    return line


def _request(stdin, request_id, method, params):
    stdin.write(
        json.dumps({"jsonrpc": "2.0", "id": request_id, "method": method, "params": params}) + "\n"
    )
    stdin.flush()


def _notify(stdin, method, params):
    stdin.write(json.dumps({"jsonrpc": "2.0", "method": method, "params": params}) + "\n")
    stdin.flush()


def _refuse(stdin, request_id, method):
    """Answer a server request we cannot serve, rather than letting it wait.

    The thread asks for no approvals and no elicitation, so nothing should call
    in. If something does, an unanswered request stalls the turn until a
    resource limit ends it, which reaches the reader as a run that stopped for
    no reason. A refusal is at least a reason.
    """
    stdin.write(
        json.dumps(
            {
                "jsonrpc": "2.0",
                "id": request_id,
                "error": {"code": -32601, "message": f"neev: {method} is not available"},
            }
        )
        + "\n"
    )
    stdin.flush()


# A thread is STARTED with a sandbox MODE (`danger-full-access`) and REPORTS a
# sandbox POLICY object (`{"type": "dangerFullAccess"}`). They are different
# shapes for the same setting, so they are compared through this table; reading
# the reply as a mode string would refuse every turn ever started.
_SANDBOX_POLICY_MODES = {
    "dangerFullAccess": "danger-full-access",
    "readOnly": "read-only",
    "workspaceWrite": "workspace-write",
}


def sandbox_mode_of(policy):
    """Pure: the mode a reported sandbox policy means, or None if unknown.

    `externalSandbox` and any policy this version does not name return None.
    Unknown is not a mismatch: refusing a turn because the reply had a shape we
    do not recognise would be inventing a problem.
    """
    if isinstance(policy, str):
        return policy
    if isinstance(policy, dict):
        return _SANDBOX_POLICY_MODES.get(policy.get("type"))
    return None


def reported_settings(result):
    """Pure: the thread's reply, in the terms the request was made in.

    Approvals are a union of a plain policy name and a granular object. Only
    the name can be compared with what was asked; a granular reply is carried
    as "not reported" rather than being flattened into a guess.
    """
    approval = result.get("approvalPolicy")
    return {
        "model": result.get("model"),
        "serviceTier": result.get("serviceTier"),
        "sandbox": sandbox_mode_of(result.get("sandbox")),
        "approvalPolicy": approval if isinstance(approval, str) else None,
    }


def settings_mismatch(asked, answered):
    """Pure: which of the reader's choices the thread did not take.

    A model or tier that silently became something else is the failure mode
    worth catching here: the turn would look like it worked, and be billed as
    the reader's choice, while running as something else. A field the server
    does not report back is not evidence of anything and is not treated as one.
    """
    wrong = []
    for field, choice in asked.items():
        if choice is None:
            continue
        got = answered.get(field)
        if got is not None and got != choice:
            wrong.append(f"{field}={got}")
    return wrong


class HandshakeTimeout(Exception):
    """The provider did not get as far as starting the turn."""


class TurnState:
    """Thread-safe state for an active turn, shared with the steering reader."""

    def __init__(self, emit):
        self._lock = threading.Lock()
        self._emit = emit
        self._thread_id = None
        self._turn_id = None
        self._finished = False
        self._next_steer_id = 100  # Request IDs for turn/steer, starting above handshake IDs

    def set_thread_id(self, thread_id):
        with self._lock:
            self._thread_id = thread_id

    def set_turn_id(self, turn_id):
        with self._lock:
            self._turn_id = turn_id

    def mark_finished(self):
        with self._lock:
            self._finished = True

    def is_active(self):
        with self._lock:
            return self._thread_id is not None and self._turn_id is not None and not self._finished

    def get_steer_params(self, text):
        """Get parameters for turn/steer, or None if not steerable."""
        with self._lock:
            if self._finished or not self._thread_id or not self._turn_id:
                return None, None
            request_id = self._next_steer_id
            self._next_steer_id += 1
            return request_id, {
                "threadId": self._thread_id,
                "expectedTurnId": self._turn_id,
                "input": [{"type": "text", "text": text}],
            }

    @property
    def turn_id(self):
        with self._lock:
            return self._turn_id


def _parse_steer_input(line):
    """Parse one steering input line. Returns (request_id, text) or raises."""
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


def _steer_reader(steer_input, provider_stdin, turn_state, emit, pending_steers):
    """Background thread: reads steering commands from steer_input, sends turn/steer.

    `pending_steers` is a dict {internal_request_id: external_request_id} tracking
    outstanding turn/steer requests so the main loop can correlate responses.
    """
    try:
        for line in steer_input:
            line = line.strip()
            if not line:
                continue
            try:
                request_id, text = _parse_steer_input(line)
            except (ValueError, json.JSONDecodeError) as error:
                # Invalid input is reported and dropped, not fatal.
                emit.steer_rejected("unknown", str(error)[:100])
                continue

            internal_id, params = turn_state.get_steer_params(text)
            if params is None:
                emit.steer_rejected(request_id, "turn not active")
                continue

            # Track this steer request for response correlation
            pending_steers[internal_id] = request_id

            try:
                _request(provider_stdin, internal_id, "turn/steer", params)
            except (BrokenPipeError, OSError):
                pending_steers.pop(internal_id, None)
                emit.steer_rejected(request_id, "provider closed")
                break
    except (OSError, ValueError):
        pass  # Input closed or invalid; the main loop handles the turn outcome


def _start_handshake_deadline(seconds=None):
    seconds = HANDSHAKE_TIMEOUT_SECS if seconds is None else seconds

    def expire(*_unused):
        raise HandshakeTimeout()

    signal.signal(signal.SIGALRM, expire)
    signal.alarm(seconds)


def _clear_handshake_deadline():
    signal.alarm(0)


def drive(child, args, emit, cwd, steer_input=None, stderr_tail=None):
    """Run the turn. Returns a process exit code.

    If `steer_input` is provided (a file-like object), steering commands are read
    from it and relayed to the provider as `turn/steer` requests. Steering is only
    available after the turn has started.
    """
    stdin, stdout = child.stdin, child.stdout
    # Turn state shared with the steering reader thread
    turn_state = TurnState(emit)
    pending_steers = {}  # {internal_request_id: external_request_id}
    steer_thread = None
    memory = {}

    # The app-server answers `initialize` before it accepts a method, so the
    # handshake is a conversation, not a queue. There is no protocol-version
    # field in the reply (the schema has none), so a well-formed result is the
    # whole of what we can honestly check.
    _start_handshake_deadline()
    _request(stdin, 1, "initialize", {"clientInfo": {"name": "neev", "version": "1"}})
    thread_id = None
    handshaken = False
    while True:
        line = _read_line(stdout)
        if line is None:
            turn_state.mark_finished()
            emit.turn_failed(*_exited(stderr_tail, "provider closed the stream", "closed"))
            return 4
        try:
            message = json.loads(line)
        except ValueError:
            continue  # chatter is not the answer
        if not isinstance(message, dict):
            continue
        if "method" in message:
            if "id" in message:
                # A request FROM the server. Refused, never ignored.
                _refuse(stdin, message["id"], message["method"])
                continue
            done = translate(message["method"], message.get("params") or {}, emit, memory)
            if done is not None:
                turn_state.mark_finished()
                return done
            continue
        if "id" not in message:
            continue
        msg_id = message["id"]
        if msg_id == RATE_LIMITS_READ_ID:
            # Best effort: an older CLI without the method, or any refusal,
            # costs nothing and never touches the turn.
            if "error" not in message:
                emit.usage(message.get("result") or {})
            continue
        if "error" in message:
            error = message["error"] or {}
            # Check if this is a steer response
            external_id = pending_steers.pop(msg_id, None)
            if external_id is not None:
                emit.steer_rejected(external_id, str(error.get("message") or "rejected")[:100])
                continue
            turn_state.mark_finished()
            # The provider's own words, unclassified here: a refusal of
            # `thread/start` may be a sign-in problem as easily as a bad value.
            emit.turn_failed(str(error.get("message") or "provider refused"))
            return 4
        result = message.get("result") or {}

        # Check if this is a turn/steer response
        external_id = pending_steers.pop(msg_id, None)
        if external_id is not None:
            # turn/steer succeeded — report the acknowledgment
            steer_turn_id = result.get("turnId") or turn_state.turn_id
            emit.steer_accepted(external_id, steer_turn_id)
            continue

        if msg_id == 1 and not handshaken:
            handshaken = True
            _notify(stdin, "initialized", {})
            _request(stdin, 2, "thread/start", thread_params(args, cwd))
        elif msg_id == 2 and thread_id is None:
            thread_id = (result.get("thread") or {}).get("id")
            if not thread_id:
                emit.turn_failed("provider started no thread")
                return 4
            turn_state.set_thread_id(thread_id)
            # The thread reports back what it actually took. A run whose model
            # or tier quietly became something else is not the run the reader
            # asked for, so it does not start.
            wrong = settings_mismatch(
                {"model": args.model, "serviceTier": args.service_tier,
                 "sandbox": SANDBOX_MODE, "approvalPolicy": APPROVAL_POLICY},
                reported_settings(result),
            )
            if wrong:
                emit.turn_failed("provider ignored: " + ", ".join(wrong), "settings_mismatch")
                return 4
            _request(stdin, 3, "turn/start", start_params(args, thread_id, cwd))
            # One read of the account's documented rate limits, for Auto. Its
            # answer (or refusal) is handled above and never fails the turn.
            _request(stdin, RATE_LIMITS_READ_ID, "account/rateLimits/read", {})
            # The turn itself is not on a clock: it may legitimately run for
            # minutes, and the launcher's own limits bound it.
            _clear_handshake_deadline()
        elif msg_id == 3:
            # turn/start response — capture the turn ID and start steering reader
            turn_id = (result.get("turn") or {}).get("id")
            if turn_id:
                turn_state.set_turn_id(turn_id)
                # Start the steering reader thread now that the turn is active
                if steer_input is not None and steer_thread is None:
                    steer_thread = threading.Thread(
                        target=_steer_reader,
                        args=(steer_input, stdin, turn_state, emit, pending_steers),
                        daemon=True,
                    )
                    steer_thread.start()
        # Other responses: turn/steer results are handled above


def main(argv=None, steer_input=None):
    """Run one Codex turn through app-server.

    `steer_input` is for testing; production uses stdin when --enable-steering.
    """
    args = _parse(sys.argv[1:] if argv is None else argv)
    cwd = args.cwd or os.getcwd()
    emit = Emitter(sys.stdout)
    if not args.prompt:
        emit.turn_failed("empty prompt")
        return 2

    # Determine steering input source
    if steer_input is not None:
        # Test injection
        effective_steer = steer_input
    elif args.enable_steering:
        # Production: use our stdin for steering commands
        effective_steer = sys.stdin
    else:
        effective_steer = None

    try:
        child = subprocess.Popen(
            codex_argv(args),
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            # Captured as a bounded tail, never forwarded on stdout (which is
            # the turn and must carry nothing else). The launcher's stderr is
            # /dev/null in production, so inheriting it threw away the only
            # explanation of a provider that died before the protocol could
            # report anything; the tail becomes that failure's reason instead.
            stderr=subprocess.PIPE,
            cwd=cwd,
            text=True,
            # No `start_new_session`: staying in the launcher's process group is
            # what lets one `killpg` stop the turn, the adapter and the provider
            # together.
        )
    except OSError:
        emit.turn_failed("provider did not start", "not_started")
        return 3
    stderr_tail = StderrTail(child.stderr)
    try:
        return drive(child, args, emit, cwd, steer_input=effective_steer,
                     stderr_tail=stderr_tail)
    except HandshakeTimeout:
        tail = stderr_tail.last(wait=0)
        emit.turn_failed(
            "provider did not start the turn" + (f": {tail}" if tail else ""), "timeout"
        )
        return 4
    except ValueError:
        # One oversized message is enough to distrust the rest of the answer.
        emit.turn_failed("oversized provider message", "oversized")
        return 4
    except OSError:
        emit.turn_failed(*_exited(stderr_tail, "provider stream ended", "closed"))
        return 4
    finally:
        _clear_handshake_deadline()
        # stderr is NOT closed here: its drain thread holds the reader, and
        # closing under it blocks until the provider exits. It ends at EOF once
        # the provider is reaped below.
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
            # Reaped, so the app-server is gone — not left as a zombie for the
            # launcher's process-group teardown to find.
            child.wait()


if __name__ == "__main__":
    sys.exit(main())
