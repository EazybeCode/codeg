"""The MCP stdio server the agent CLI spawns.

It exposes exactly six tools and forwards each call over the per-run socket.
There is no tool that evaluates JavaScript, navigates, opens tabs, or reads
storage — the surface is the allowlist and nothing else.
"""

import json
import os
import re
import sys

from .bounds import bound_snapshot
from .ipc import AuthError, IpcClient
from .scope import ALLOWED_TOOLS

PROTOCOL_VERSION = "2024-11-05"
SERVER_NAME = "neev_browser"
SERVER_VERSION = "0.1.0"
MAX_TYPE_CHARS = 2000
MAX_SCREENSHOT_BASE64 = 600000

_NO_ARGS = {"type": "object", "properties": {}, "additionalProperties": False}
_REF = {"type": "string", "pattern": "^e[0-9]+:[0-9]+$",
        "description": "An element reference from the most recent "
                       "browser_snapshot. References expire when the page "
                       "changes."}

TOOL_SCHEMAS = [
    {"name": "browser_snapshot",
     "description": "Read the attached tab: its URL, title, and a bounded list "
                    "of interactive and text elements with references you can "
                    "act on. Field contents are never returned.",
     "inputSchema": _NO_ARGS},
    {"name": "browser_click",
     "description": "Click one element. If it is consequential (a submit "
                    "control, a form, or a link leaving this site) the person "
                    "is asked to approve first, and may decline.",
     "inputSchema": {"type": "object", "properties": {"ref": _REF},
                     "required": ["ref"], "additionalProperties": False}},
    {"name": "browser_type",
     "description": "Type text into one field. This does not press Enter and "
                    "does not submit the form.",
     "inputSchema": {"type": "object",
                     "properties": {"ref": _REF,
                                    "text": {"type": "string",
                                             "maxLength": MAX_TYPE_CHARS}},
                     "required": ["ref", "text"], "additionalProperties": False}},
    {"name": "browser_scroll",
     "description": "Scroll the page, or bring one element into view.",
     "inputSchema": {"type": "object",
                     "properties": {
                         "direction": {"type": "string", "enum": ["up", "down"]},
                         "amount": {"type": "integer", "minimum": 1, "maximum": 10},
                         "ref": _REF},
                     "additionalProperties": False}},
    {"name": "browser_screenshot",
     "description": "Capture the visible area of the attached tab as a JPEG.",
     "inputSchema": _NO_ARGS},
    {"name": "browser_request_user_action",
     "description": "Ask the person to do something you cannot: navigate, sign "
                    "in, pay, or use browser UI. Explain what you need and why. "
                    "They will tell you whether they did it.",
     "inputSchema": {"type": "object",
                     "properties": {
                         "reason": {"type": "string", "maxLength": 300},
                         "instruction": {"type": "string", "maxLength": 500}},
                     "required": ["reason", "instruction"],
                     "additionalProperties": False}},
]


MAX_LINE_BYTES = 262144
_REF_RE = re.compile(r"^e[0-9]+:[0-9]+$")
_APPROVALS = ("not_required", "approved", "denied")


def _is_int(value):
    return isinstance(value, int) and not isinstance(value, bool)


def _text(value, limit):
    return isinstance(value, str) and 0 < len(value) <= limit


def validate_arguments(name, args):
    """Check a tool call before it touches the socket.

    The inputSchema is advisory — nothing guarantees the CLI enforced it — so
    the same rules are applied here, where refusing actually stops the call.
    Returns None when the arguments are acceptable, or a message.
    """
    if not isinstance(args, dict):
        return "arguments must be an object"
    allowed = {
        "browser_snapshot": (),
        "browser_screenshot": (),
        "browser_click": ("ref",),
        "browser_type": ("ref", "text"),
        "browser_scroll": ("direction", "amount", "ref"),
        "browser_request_user_action": ("reason", "instruction"),
    }[name]
    extra = sorted(set(args) - set(allowed))
    if extra:
        return "unexpected argument(s): %s" % ", ".join(extra)

    if name in ("browser_click", "browser_type"):
        if not isinstance(args.get("ref"), str) or not _REF_RE.match(args["ref"]):
            return "ref must look like e<epoch>:<index> from a recent snapshot"
    if name == "browser_type":
        text = args.get("text")
        if not isinstance(text, str):
            return "text must be a string"
        if len(text) > MAX_TYPE_CHARS:
            return "text exceeds %d characters" % MAX_TYPE_CHARS
    if name == "browser_scroll":
        ref, direction, amount = (args.get("ref"), args.get("direction"),
                                  args.get("amount"))
        if ref is None and direction is None:
            return "give either a direction or a ref to scroll to"
        if ref is not None and (not isinstance(ref, str) or not _REF_RE.match(ref)):
            return "ref must look like e<epoch>:<index> from a recent snapshot"
        if direction is not None and direction not in ("up", "down"):
            return 'direction must be "up" or "down"'
        if amount is not None and (not _is_int(amount) or not 1 <= amount <= 10):
            return "amount must be a whole number from 1 to 10"
    if name == "browser_request_user_action":
        if not _text(args.get("reason"), 300):
            return "reason must be a string of 1..300 characters"
        if not _text(args.get("instruction"), 500):
            return "instruction must be a string of 1..500 characters"
    return None


def _text_result(payload, is_error=False):
    return {"content": [{"type": "text",
                         "text": payload if isinstance(payload, str)
                         else json.dumps(payload, sort_keys=True)}],
            "isError": is_error}


class McpServer(object):
    def __init__(self, in_stream, out_stream, client):
        self._in = in_stream
        self._out = out_stream
        self._client = client

    # ------------------------------------------------------------- dispatch

    def handle(self, request):
        method = request.get("method")
        rid = request.get("id")
        if method == "initialize":
            result = {"protocolVersion": PROTOCOL_VERSION,
                      "capabilities": {"tools": {}},
                      "serverInfo": {"name": SERVER_NAME, "version": SERVER_VERSION}}
        elif method == "tools/list":
            result = {"tools": TOOL_SCHEMAS}
        elif method == "tools/call":
            result = self._call(request.get("params") or {})
        elif method == "ping":
            result = {}
        else:
            return {"jsonrpc": "2.0", "id": rid,
                    "error": {"code": -32601, "message": "unknown method %r" % (method,)}}
        return {"jsonrpc": "2.0", "id": rid, "result": result}

    def _call(self, params):
        name = params.get("name")
        if name not in ALLOWED_TOOLS:
            # Refused here, before the socket is touched at all.
            return _text_result("%r is not an available tool" % (name,), True)
        args = params.get("arguments")
        args = args if args is not None else {}
        problem = validate_arguments(name, args)
        if problem is not None:
            return _text_result(problem, True)
        try:
            reply = self._client.call({"tool": name, "args": args})
        except (AuthError, OSError) as exc:
            return _text_result("the browser connection closed: %s" % exc, True)
        if not isinstance(reply, dict):
            return _text_result("the host returned an unusable reply", True)
        if reply.get("error"):
            message = reply.get("message")
            return _text_result(
                "%s%s" % (reply["error"], ": %s" % message if message else ""), True)
        return self._render(name, reply)

    @staticmethod
    def _render(name, reply):
        """Turn a browser reply into a tool result, or into an honest error.

        A reply that does not carry what the tool promised is reported as a
        failure. Nothing shapeless is allowed to read as success.
        """
        if name == "browser_screenshot":
            data = reply.get("dataBase64")
            if not isinstance(data, str) or not data:
                return _text_result("no screenshot was returned", True)
            if len(data) > MAX_SCREENSHOT_BASE64:
                return _text_result("the screenshot was too large to return", True)
            return {"content": [{"type": "image", "data": data,
                                 "mimeType": "image/jpeg"}], "isError": False}

        snapshot = reply.get("snapshot")
        if not isinstance(snapshot, dict):
            return _text_result("the browser returned no page reading", True)
        payload = {"snapshot": bound_snapshot(snapshot)}

        if name in ("browser_click", "browser_type", "browser_scroll"):
            if not isinstance(reply.get("ok"), bool):
                return _text_result("the browser did not say whether the action "
                                    "happened", True)
            payload["ok"] = reply["ok"]
        if name in ("browser_click", "browser_type"):
            approval = reply.get("approval")
            if approval not in _APPROVALS:
                return _text_result("the browser returned an unusable approval "
                                    "state", True)
            payload["approval"] = approval
        if name == "browser_request_user_action":
            outcome = reply.get("outcome")
            if outcome not in ("done", "skipped"):
                return _text_result("the person's answer was not recorded", True)
            payload["outcome"] = outcome
        return _text_result(payload)

    # ----------------------------------------------------------------- loop

    def _read_line(self):
        """Read one bounded line. An over-long line is discarded, not parsed."""
        line = self._in.readline(MAX_LINE_BYTES)
        if not line:
            return None
        if not line.endswith(b"\n"):
            while True:
                more = self._in.readline(MAX_LINE_BYTES)
                if not more or more.endswith(b"\n"):
                    break
            return b""
        return line

    def serve_forever(self):
        while True:
            line = self._read_line()
            if line is None:
                return
            line = line.strip()
            if not line:
                continue
            try:
                request = json.loads(line.decode("utf-8"))
            except ValueError:
                continue
            if not isinstance(request, dict):
                continue
            if request.get("id") is None and "method" in request:
                continue                                    # a notification
            response = self.handle(request)
            self._out.write(json.dumps(response).encode("utf-8") + b"\n")
            self._out.flush()


def main():
    socket_path = os.environ.get("NEEV_BROWSER_SOCKET")
    token = os.environ.get("NEEV_BROWSER_TOKEN")
    if not socket_path or not token:
        sys.stderr.write("NEEV_BROWSER_SOCKET and NEEV_BROWSER_TOKEN are required\n")
        return 2
    try:
        client = IpcClient(socket_path, token, timeout=930)
    except (AuthError, OSError) as exc:
        sys.stderr.write("cannot reach the Neev Browser host: %s\n" % exc)
        return 3
    server = McpServer(getattr(sys.stdin, "buffer", sys.stdin),
                       getattr(sys.stdout, "buffer", sys.stdout), client)
    try:
        server.serve_forever()
    finally:
        client.close()
    return 0


if __name__ == "__main__":
    sys.exit(main())
