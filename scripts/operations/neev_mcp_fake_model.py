#!/usr/bin/env python3
"""ACCEPTANCE ONLY: a scripted loopback model so a pinned CLI can be driven to
make ONE brokered MCP tool call without any provider credential or network.

    python3 -I neev_mcp_fake_model.py --port P --marker neev-accept-marker-<hex> \
        [--tool mcp__accept__echo] [--timeout 240] -- <cli argv...>

It serves on 127.0.0.1:P (the run netns' own loopback), runs the CLI as a child,
forwards the child's stdout line by line, and when the child exits prints ONE
summary line `{"type":"neev.fake_model", ...}` (booleans and counts, never a
request body) and exits with the child's code.

Dialects (stdlib only, stateless per request):
  * Anthropic Messages  POST /v1/messages[?beta=true]  (SSE when "stream": true)
  * OpenAI Responses    POST /v1/responses             (SSE when "stream": true)
  * OpenAI Chat         POST /v1/chat/completions      (SSE when "stream": true)
  * GET /v1/models, POST /v1/messages/count_tokens

Script `echo`: when the request offers the tool and does not yet carry the
result of OUR scripted call, answer with exactly one tool call
`<tool>({"text": <marker>})`; once the result of that call is present, answer
with a final text turn. Any other request (titles, quota checks) gets a short
text answer. The summary records whether the scripted call was issued and
whether its result came back carrying `echo:<marker>` — model-side evidence
that complements the CLI's own parsed event stream.

Never a product file: it is not referenced by the launcher, the runtime or any
production path, and it holds no secret (the CLI's API key is a fixed dummy).
"""

import argparse
import json
import signal
import subprocess
import sys
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import urlsplit

TOOL_USE_ID = "toolu_neevaccept01"
CALL_ID = "call_neevaccept01"
MAX_BODY_BYTES = 32 * 1024 * 1024
MAX_TOOL_NAMES = 64
FINAL_TEXT = "neev-accept: tool result received"
SIDE_TEXT = "ok"


class State:
    """What the fake model saw. Thread-safe; only booleans, counts and tool names."""

    def __init__(self, tool, marker):
        self.tool = tool
        self.marker = marker
        self.lock = threading.Lock()
        self.requests = 0
        self.dialects = {}
        self.tool_call_issued = 0
        self.tool_call_name = None
        self.tool_result_seen = False
        self.tool_result_has_marker = False
        self.tool_result_is_error = False
        self.final_text_sent = 0
        self.offered = []

    def note(self, dialect, offered):
        with self.lock:
            self.requests += 1
            self.dialects[dialect] = self.dialects.get(dialect, 0) + 1
            for name in offered:
                if name not in self.offered and len(self.offered) < MAX_TOOL_NAMES:
                    self.offered.append(name[:128])

    def summary(self):
        with self.lock:
            return {
                "type": "neev.fake_model",
                "requests": self.requests,
                "dialects": dict(self.dialects),
                "toolCallIssued": self.tool_call_issued,
                "toolCallName": self.tool_call_name,
                "toolResultSeen": self.tool_result_seen,
                "toolResultHasMarker": self.tool_result_has_marker,
                "toolResultIsError": self.tool_result_is_error,
                "finalTextSent": self.final_text_sent,
                "offeredTools": list(self.offered),
            }


def _strings(value, depth=0):
    """Every string inside a JSON value; strings that are themselves JSON are
    decoded and searched too (Codex may carry a tool result as a JSON string)."""
    if depth > 8:
        return []
    if isinstance(value, str):
        out = [value]
        text = value.strip()
        if text[:1] in ("{", "["):
            try:
                out += _strings(json.loads(text), depth + 1)
            except ValueError:
                pass
        return out
    if isinstance(value, dict):
        return [s for item in value.values() for s in _strings(item, depth + 1)]
    if isinstance(value, list):
        return [s for item in value for s in _strings(item, depth + 1)]
    return []


def carries_marker(value, marker):
    """Pure: does any text piece of a tool result equal `echo:<marker>`?"""
    want = "echo:" + marker
    return any(piece.strip() == want for piece in _strings(value))


def pick_tool(offered, wanted):
    """Pure: the offered tool name to call — the exact name, else a name that is
    clearly the same server/tool under another qualification (recorded)."""
    if wanted in offered:
        return wanted
    server, _, tool = wanted[len("mcp__"):].partition("__") if wanted.startswith("mcp__") \
        else ("", "", wanted)
    for name in offered:
        flat = name.replace(".", "__").replace("/", "__")
        if server and tool and flat.endswith("__" + tool) and server in flat:
            return name
    return None


# --------------------------------------------------------------------------- #
# Anthropic Messages
# --------------------------------------------------------------------------- #


def anthropic_tool_names(body):
    tools = body.get("tools") if isinstance(body, dict) else None
    return [t["name"] for t in tools or [] if isinstance(t, dict) and isinstance(t.get("name"), str)]


def anthropic_tool_result(body):
    """The tool_result block answering OUR scripted tool_use, or None."""
    for message in body.get("messages") or []:
        content = message.get("content") if isinstance(message, dict) else None
        if not isinstance(content, list):
            continue
        for block in content:
            if (isinstance(block, dict) and block.get("type") == "tool_result"
                    and block.get("tool_use_id") == TOOL_USE_ID):
                return block
    return None


def anthropic_plan(body, state):
    """Pure-ish: ("tool_use", name) | ("text", text), updating the evidence."""
    offered = anthropic_tool_names(body)
    state.note("anthropic", offered)
    result = anthropic_tool_result(body)
    if result is not None:
        with state.lock:
            state.tool_result_seen = True
            state.tool_result_is_error = state.tool_result_is_error or bool(result.get("is_error"))
            if carries_marker(result.get("content"), state.marker) and not result.get("is_error"):
                state.tool_result_has_marker = True
            state.final_text_sent += 1
        return ("text", FINAL_TEXT)
    name = pick_tool(offered, state.tool)
    if name is not None:
        with state.lock:
            state.tool_call_issued += 1
            state.tool_call_name = name
        return ("tool_use", name)
    return ("text", SIDE_TEXT)


def anthropic_message(plan, model, marker):
    kind, value = plan[0], plan[1]
    if kind == "tool_use":
        content = [{"type": "tool_use", "id": TOOL_USE_ID, "name": value,
                    "input": {"text": marker}}]
        stop = "tool_use"
    else:
        content = [{"type": "text", "text": value}]
        stop = "end_turn"
    return {"id": "msg_neevaccept", "type": "message", "role": "assistant", "model": model,
            "content": content, "stop_reason": stop, "stop_sequence": None,
            "usage": {"input_tokens": 1, "output_tokens": 1}}


def anthropic_events(plan, model, marker):
    """The SSE event list for one streamed Messages response."""
    kind, value = plan[0], plan[1]
    start = anthropic_message(plan, model, marker)
    start = dict(start, content=[], stop_reason=None)
    events = [("message_start", {"type": "message_start", "message": start})]
    if kind == "tool_use":
        events += [
            ("content_block_start", {"type": "content_block_start", "index": 0, "content_block": {
                "type": "tool_use", "id": TOOL_USE_ID, "name": value, "input": {}}}),
            ("content_block_delta", {"type": "content_block_delta", "index": 0, "delta": {
                "type": "input_json_delta", "partial_json": json.dumps({"text": marker})}}),
        ]
        stop = "tool_use"
    else:
        events += [
            ("content_block_start", {"type": "content_block_start", "index": 0,
                                     "content_block": {"type": "text", "text": ""}}),
            ("content_block_delta", {"type": "content_block_delta", "index": 0,
                                     "delta": {"type": "text_delta", "text": value}}),
        ]
        stop = "end_turn"
    events += [
        ("content_block_stop", {"type": "content_block_stop", "index": 0}),
        ("message_delta", {"type": "message_delta", "delta": {"stop_reason": stop,
                                                              "stop_sequence": None},
                           "usage": {"output_tokens": 1}}),
        ("message_stop", {"type": "message_stop"}),
    ]
    return events


# --------------------------------------------------------------------------- #
# OpenAI Responses and Chat Completions
# --------------------------------------------------------------------------- #


def openai_tool_names(body):
    """Offered tool names. A Responses `namespace` tool (how the pinned Codex
    offers an MCP server's tools: `{"type":"namespace","name":"mcp__accept",
    "tools":[{"type":"function","name":"echo"},…]}`) contributes each nested
    function as `<namespace>__<name>`; `openai_namespaces` maps those back."""
    return list(_openai_tools(body))


def openai_namespaces(body):
    """Pure: flat `<namespace>__<name>` -> (namespace, name) for nested tools."""
    return {flat: split for flat, split in _openai_tools(body).items() if split is not None}


def _openai_tools(body):
    found = {}
    for tool in (body.get("tools") if isinstance(body, dict) else None) or []:
        if not isinstance(tool, dict):
            continue
        if tool.get("type") == "namespace" and isinstance(tool.get("name"), str):
            for child in tool.get("tools") or []:
                if not isinstance(child, dict):
                    continue
                child_name = child.get("name")
                if not isinstance(child_name, str) and isinstance(child.get("function"), dict):
                    child_name = child["function"].get("name")
                if isinstance(child_name, str):
                    found["%s__%s" % (tool["name"], child_name)] = (tool["name"], child_name)
            continue
        name = tool.get("name")
        if not isinstance(name, str) and isinstance(tool.get("function"), dict):
            name = tool["function"].get("name")
        if isinstance(name, str):
            found[name] = None
    return found


def responses_tool_output(body):
    for item in body.get("input") or []:
        if (isinstance(item, dict) and item.get("type") == "function_call_output"
                and item.get("call_id") == CALL_ID):
            return item
    return None


def chat_tool_output(body):
    for message in body.get("messages") or []:
        if (isinstance(message, dict) and message.get("role") == "tool"
                and message.get("tool_call_id") == CALL_ID):
            return message
    return None


def openai_plan(body, state, dialect):
    offered = openai_tool_names(body)
    state.note(dialect, offered)
    output = responses_tool_output(body) if dialect == "responses" else chat_tool_output(body)
    if output is not None:
        payload = output.get("output") if dialect == "responses" else output.get("content")
        with state.lock:
            state.tool_result_seen = True
            if carries_marker(payload, state.marker):
                state.tool_result_has_marker = True
            state.final_text_sent += 1
        return ("text", FINAL_TEXT)
    name = pick_tool(offered, state.tool)
    if name is not None:
        with state.lock:
            state.tool_call_issued += 1
            state.tool_call_name = name
        split = openai_namespaces(body).get(name)
        if split is not None and dialect == "responses":
            # Answer in the shape the tool was offered: Codex's FunctionCall
            # carries `namespace` + the bare `name`.
            return ("tool_use", name, split)
        return ("tool_use", name)
    return ("text", SIDE_TEXT)


_USAGE = {"input_tokens": 1, "input_tokens_details": None, "output_tokens": 1,
          "output_tokens_details": None, "total_tokens": 2}


def responses_items(plan, marker):
    kind, value = plan[0], plan[1]
    if kind == "tool_use":
        call = {"type": "function_call", "id": "fc_neevaccept01", "call_id": CALL_ID,
                "name": value, "arguments": json.dumps({"text": marker}),
                "status": "completed"}
        if len(plan) > 2 and plan[2] is not None:
            call["namespace"], call["name"] = plan[2]
        return [call]
    return [{"type": "message", "id": "msg_neevaccept", "role": "assistant", "status": "completed",
             "content": [{"type": "output_text", "text": value, "annotations": []}]}]


def responses_events(plan, model, marker):
    items = responses_items(plan, marker)
    base = {"id": "resp_neevaccept", "object": "response", "model": model, "output": []}
    events = [("response.created", {"type": "response.created", "response": base})]
    for index, item in enumerate(items):
        events.append(("response.output_item.added", {
            "type": "response.output_item.added", "output_index": index, "item": item}))
        events.append(("response.output_item.done", {
            "type": "response.output_item.done", "output_index": index, "item": item}))
    events.append(("response.completed", {"type": "response.completed", "response": dict(
        base, status="completed", output=items, usage=_USAGE)}))
    return events


def responses_object(plan, model, marker):
    return {"id": "resp_neevaccept", "object": "response", "model": model, "status": "completed",
            "output": responses_items(plan, marker), "usage": _USAGE}


def chat_object(plan, model, marker):
    kind, value = plan[0], plan[1]
    if kind == "tool_use":
        message = {"role": "assistant", "content": None, "tool_calls": [{
            "id": CALL_ID, "type": "function",
            "function": {"name": value, "arguments": json.dumps({"text": marker})}}]}
        finish = "tool_calls"
    else:
        message = {"role": "assistant", "content": value}
        finish = "stop"
    return {"id": "chatcmpl-neevaccept", "object": "chat.completion", "model": model,
            "choices": [{"index": 0, "message": message, "finish_reason": finish}],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}}


def chat_chunks(plan, model, marker):
    kind, value = plan[0], plan[1]
    head = {"id": "chatcmpl-neevaccept", "object": "chat.completion.chunk", "model": model}
    if kind == "tool_use":
        delta = {"role": "assistant", "tool_calls": [{
            "index": 0, "id": CALL_ID, "type": "function",
            "function": {"name": value, "arguments": json.dumps({"text": marker})}}]}
        finish = "tool_calls"
    else:
        delta = {"role": "assistant", "content": value}
        finish = "stop"
    return [dict(head, choices=[{"index": 0, "delta": delta, "finish_reason": None}]),
            dict(head, choices=[{"index": 0, "delta": {}, "finish_reason": finish}])]


# --------------------------------------------------------------------------- #
# HTTP
# --------------------------------------------------------------------------- #


def make_handler(state):
    class Handler(BaseHTTPRequestHandler):
        protocol_version = "HTTP/1.1"
        server_version = "neev-fake-model"

        def log_message(self, *_args):  # never echo requests anywhere
            pass

        def _json(self, status, payload):
            data = json.dumps(payload).encode("utf-8")
            self.send_response(status)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)

        def _sse(self, events, done=False):
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.send_header("Cache-Control", "no-cache")
            self.send_header("Connection", "close")
            self.end_headers()
            self.close_connection = True
            for name, payload in events:
                frame = ("event: %s\n" % name if name else "") + "data: %s\n\n" % json.dumps(payload)
                self.wfile.write(frame.encode("utf-8"))
                self.wfile.flush()
            if done:
                self.wfile.write(b"data: [DONE]\n\n")
                self.wfile.flush()

        def _body(self):
            try:
                length = int(self.headers.get("Content-Length") or 0)
            except ValueError:
                length = 0
            if length <= 0 or length > MAX_BODY_BYTES:
                return None
            try:
                value = json.loads(self.rfile.read(length))
            except ValueError:
                return None
            return value if isinstance(value, dict) else None

        def do_GET(self):  # noqa: N802
            path = urlsplit(self.path).path.rstrip("/")
            if path.endswith("/models"):
                self._json(200, {"object": "list", "data": [
                    {"id": "neev-fake", "object": "model", "created": 0, "owned_by": "neev"}],
                    "models": []})
            else:
                self._json(404, {"error": {"type": "not_found", "message": "not found"}})

        def do_HEAD(self):  # noqa: N802
            self.send_response(200)
            self.send_header("Content-Length", "0")
            self.end_headers()

        def do_POST(self):  # noqa: N802
            path = urlsplit(self.path).path.rstrip("/")
            body = self._body()
            if body is None:
                self._json(400, {"error": {"type": "invalid_request_error", "message": "bad body"}})
                return
            model = body.get("model") if isinstance(body.get("model"), str) else "neev-fake"
            stream = body.get("stream") is True
            if path.endswith("/messages/count_tokens"):
                self._json(200, {"input_tokens": 1})
            elif path.endswith("/messages"):
                plan = anthropic_plan(body, state)
                if stream:
                    self._sse(anthropic_events(plan, model, state.marker))
                else:
                    self._json(200, anthropic_message(plan, model, state.marker))
            elif path.endswith("/responses"):
                plan = openai_plan(body, state, "responses")
                if stream:
                    self._sse(responses_events(plan, model, state.marker))
                else:
                    self._json(200, responses_object(plan, model, state.marker))
            elif path.endswith("/chat/completions"):
                plan = openai_plan(body, state, "chat")
                if stream:
                    self._sse([(None, chunk) for chunk in chat_chunks(plan, model, state.marker)],
                              done=True)
                else:
                    self._json(200, chat_object(plan, model, state.marker))
            else:
                self._json(404, {"error": {"type": "not_found", "message": "not found"}})

    return Handler


def make_server(port, marker, tool="mcp__accept__echo", host="127.0.0.1"):
    """A started loopback server (daemon thread) and its State."""
    state = State(tool, marker)
    server = ThreadingHTTPServer((host, port), make_handler(state))
    server.daemon_threads = True
    threading.Thread(target=server.serve_forever, daemon=True).start()
    return server, state


def _parse(argv):
    parser = argparse.ArgumentParser(prog="neev_mcp_fake_model")
    parser.add_argument("--port", type=int, required=True)
    parser.add_argument("--marker", required=True)
    parser.add_argument("--tool", default="mcp__accept__echo")
    parser.add_argument("--script", default="echo", choices=("echo",))
    parser.add_argument("--timeout", type=int, default=240)
    parser.add_argument("cli", nargs=argparse.REMAINDER)
    args = parser.parse_args(argv)
    cli = list(args.cli)
    if cli and cli[0] == "--":
        cli = cli[1:]
    if not cli:
        parser.error("a CLI argv is required after --")
    if not args.marker.startswith("neev-accept-marker-") or len(args.marker) > 128:
        parser.error("marker must be neev-accept-marker-<random>")
    args.cli = cli
    return args


def main(argv=None, out=None):
    args = _parse(sys.argv[1:] if argv is None else argv)
    out = out if out is not None else sys.stdout.buffer
    server, state = make_server(args.port, args.marker, args.tool)
    try:
        child = subprocess.Popen(args.cli, stdout=subprocess.PIPE, close_fds=True)
    except OSError:
        out.write((json.dumps(dict(state.summary(), childStarted=False)) + "\n").encode())
        out.flush()
        server.shutdown()
        return 3
    timed_out = []

    def expire():
        timed_out.append(True)
        try:
            child.kill()
        except OSError:
            pass

    timer = threading.Timer(args.timeout, expire)
    timer.daemon = True
    timer.start()

    def stop(*_unused):
        try:
            child.kill()
        except OSError:
            pass

    signal.signal(signal.SIGTERM, stop)
    try:
        for line in child.stdout:
            out.write(line if line.endswith(b"\n") else line + b"\n")
            out.flush()
        rc = child.wait()
    finally:
        timer.cancel()
        server.shutdown()
    summary = dict(state.summary(), childStarted=True, childExit=rc, timedOut=bool(timed_out))
    out.write((json.dumps(summary, separators=(",", ":")) + "\n").encode("utf-8"))
    out.flush()
    return rc if 0 <= rc < 256 else 1


if __name__ == "__main__":
    raise SystemExit(main())
