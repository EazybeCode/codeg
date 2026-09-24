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
import signal
import subprocess
import sys

# One app-server message. Comfortably above a real delta or command output
# (which the server chunks) and far below anything that could exhaust memory.
MAX_LINE_BYTES = 1 * 1024 * 1024
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

    def turn_failed(self, reason):
        self._write({"type": "turn.failed", "reason": reason})


def translate(method, params, emit):
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
    if method == "turn/completed":
        turn = params.get("turn") or {}
        status = turn.get("status")
        if status == "completed":
            emit.turn_completed()
            return 0
        # `interrupted` and `failed` both mean the reply is not finished. The
        # EXIT CODE is what settles the turn upstream, so a partial reply must
        # never leave here as a success.
        error = (turn.get("error") or {}).get("message") or status or "failed"
        emit.turn_failed(str(error)[:200])
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


def _start_handshake_deadline(seconds=None):
    seconds = HANDSHAKE_TIMEOUT_SECS if seconds is None else seconds

    def expire(*_unused):
        raise HandshakeTimeout()

    signal.signal(signal.SIGALRM, expire)
    signal.alarm(seconds)


def _clear_handshake_deadline():
    signal.alarm(0)


def drive(child, args, emit, cwd):
    """Run the turn. Returns a process exit code."""
    stdin, stdout = child.stdin, child.stdout
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
            emit.turn_failed("provider closed the stream")
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
            done = translate(message["method"], message.get("params") or {}, emit)
            if done is not None:
                return done
            continue
        if "id" not in message:
            continue
        if "error" in message:
            error = message["error"] or {}
            emit.turn_failed(str(error.get("message") or "provider refused")[:200])
            return 4
        result = message.get("result") or {}
        if message["id"] == 1 and not handshaken:
            handshaken = True
            _notify(stdin, "initialized", {})
            _request(stdin, 2, "thread/start", thread_params(args, cwd))
        elif message["id"] == 2 and thread_id is None:
            thread_id = (result.get("thread") or {}).get("id")
            if not thread_id:
                emit.turn_failed("provider started no thread")
                return 4
            # The thread reports back what it actually took. A run whose model
            # or tier quietly became something else is not the run the reader
            # asked for, so it does not start.
            wrong = settings_mismatch(
                {"model": args.model, "serviceTier": args.service_tier,
                 "sandbox": SANDBOX_MODE, "approvalPolicy": APPROVAL_POLICY},
                reported_settings(result),
            )
            if wrong:
                emit.turn_failed("provider ignored: " + ", ".join(wrong))
                return 4
            _request(stdin, 3, "turn/start", start_params(args, thread_id, cwd))
            # The turn itself is not on a clock: it may legitimately run for
            # minutes, and the launcher's own limits bound it.
            _clear_handshake_deadline()
        # A `turn/start` reply carries the turn, whose items then arrive as
        # notifications; there is nothing to do with it here.


def main(argv=None):
    args = _parse(sys.argv[1:] if argv is None else argv)
    cwd = args.cwd or os.getcwd()
    emit = Emitter(sys.stdout)
    if not args.prompt:
        emit.turn_failed("empty prompt")
        return 2
    try:
        child = subprocess.Popen(
            codex_argv(args),
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            # Inherited, NOT discarded: our stdout is the turn and must carry
            # nothing else, but whatever the provider says about why it could
            # not start belongs wherever the launcher's stderr goes (/dev/null
            # in production, the terminal under the diagnostics).
            cwd=cwd,
            text=True,
            # No `start_new_session`: staying in the launcher's process group is
            # what lets one `killpg` stop the turn, the adapter and the provider
            # together.
        )
    except OSError:
        emit.turn_failed("provider did not start")
        return 3
    try:
        return drive(child, args, emit, cwd)
    except HandshakeTimeout:
        emit.turn_failed("provider did not start the turn")
        return 4
    except ValueError:
        # One oversized message is enough to distrust the rest of the answer.
        emit.turn_failed("oversized provider message")
        return 4
    except OSError:
        emit.turn_failed("provider stream ended")
        return 4
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
            # Reaped, so the app-server is gone — not left as a zombie for the
            # launcher's process-group teardown to find.
            child.wait()


if __name__ == "__main__":
    sys.exit(main())
