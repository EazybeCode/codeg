#!/usr/bin/env python3
"""Run one turn of an ACP agent (Cursor, Gemini CLI, OpenCode) inside the sandbox.

Launched by `neev_agent_launch.acp_stream_argv` (runs) and by the supervisor's
account probe (`--probe`). NOT live for any provider until a real per-provider
sign-in + run + stream + cancel smoke passes on the pinned image and the
coordinator's `NEEV_AGENT_ACP_PROVIDERS` gate names it.

Protocol: Agent Client Protocol v1 over stdio JSON-RPC, pinned to the official
schema `schema-v1.23.0` (agentclientprotocol/agent-client-protocol). The
adapter is the ACP *client*: `initialize` -> `session/new` -> `session/prompt`,
reading `session/update` notifications until the prompt returns a
`stopReason`.

Output is the SAME dialect `neev_codex_stream_adapter.py` writes, so the Rust
parser's existing Codex mapping reads it unchanged:

  {"type":"item.delta","item":{"type":"agent_message","id":..,"delta":..}}
  {"type":"item.completed","item":{"type":"agent_message","id":..,"text":..}}
  {"type":"item.started","item":{"type":"command_execution","id":..,"command":..}}
  {"type":"item.completed","item":{"type":"command_execution","id":..,
      "command":..,"aggregated_output":..,"exit_code":0|1}}
  {"type":"turn.completed"} | {"type":"turn.failed","reason":<fixed string>}
  {"type":"neev.steer","requestId":..,"accepted":false,"reason":..}

Rules:
- Thought chunks (`agent_thought_chunk`) are dropped, never written: the Neev
  activity contract stores no hidden reasoning.
- Permissions fail CLOSED. `reject` (default) answers every
  `session/request_permission` with a `reject_once` option, or `cancelled` when
  none is offered. `allow-once` is only for a parent that already holds a
  product grant; it never selects `allow_always` (that would persist a rule in
  the user's HOME past this run).
- No fs/terminal client capabilities are advertised; any client-side request
  is refused with -32601 rather than left to stall the turn.
- ACP v1 has no mid-turn follow-up. Steering input, if enabled, is rejected
  per request. Stop is SIGTERM/SIGINT -> `session/cancel`, bounded grace.
- Credentials: the adapter never reads HOME or any credential file. The child
  gets an env ALLOWLIST (HOME from the parent, locale, PATH, proxy); API-key
  and token variables are never forwarded, so no silent key path exists.
- Errors on stdout are fixed strings. Provider error text is never relayed:
  it can carry paths, account names or tokens.
- Bounded: line size, queue depth, open tool count, per-field lengths.
- OpenCode answers `session/new` even with NO sign-in, on its own anonymous
  "OpenCode Zen" models (observed, pinned 1.18.33). A run must therefore be pinned
  to the signed-in upstream: the model config option is moved to an `openai/`
  model via `session/set_config_option`, or the turn fails "provider sign-in
  required". It never runs on a model the user did not connect.
- Gemini, with Google sign-in selected but no cached credential, prints its
  NO_BROWSER sign-in prompt on stdout and blocks `session/new` waiting for a code
  (observed, pinned 0.62.0). That prompt is recognised and the turn fails fast
  "provider sign-in required"; the URL is never relayed.

`--probe` answers "is this user's provider session usable?" without a prompt (no
tokens spent): `initialize` + `session/new` (+ the model pin above), then prints
ONE line `{"type":"neev.account","status":"supported"|"disconnected"}` and exits 0.
Anything it cannot decide exits 4 with `status:"unavailable"` — never a guess.
"""

import argparse
import json
import os
import queue
import re
import signal
import subprocess
import sys
import threading
import time

PROTOCOL_VERSION = 1
SCHEMA_PIN = "schema-v1.23.0"

MAX_LINE_BYTES = 16 * 1024 * 1024
MAX_QUEUE_LINES = 1024
MAX_OPEN_TOOLS = 256
MAX_TITLE_CHARS = 512
MAX_TOOL_PREVIEW_CHARS = 4096
MAX_MESSAGE_CHARS = 1024 * 1024
HANDSHAKE_TIMEOUT_SECS = 60
CANCEL_GRACE_SECS = 10
POLL_SECS = 0.2

STEER_MAX_LINE_BYTES = 64 * 1024
FOLLOW_UP_UNSUPPORTED = "follow-up not supported by provider"

# Per provider: the ONLY argv the agent is started with, and the executable
# basenames the parent may allowlist. Official docs: cursor.com/docs/cli/acp
# (`agent acp`), geminicli.com/docs/cli/acp-mode (`gemini --acp`),
# opencode.ai/docs/acp (`opencode acp`).
#
# `env` is FIXED (never read from the parent) and matches
# neev_agent_credentials.provider_env: each CLI's official headless switch, so no
# browser launch is attempted inside the sandbox.
# `signin_markers`: stdout text a provider prints INSTEAD of protocol when it
# wants an interactive sign-in (source-verified, see module docstring).
# `model_prefix`: the model id prefix of the upstream Neev signs the user into.
OPENCODE_LOOPBACK = "127.0.0.1,localhost,::1"
PROVIDERS = {
    "cursor": {"args": ("acp",), "binaries": ("agent", "cursor-agent"),
               "env": {"NO_OPEN_BROWSER": "1", "AGENT_CLI_CREDENTIAL_STORE": "file"},
               "signin_markers": (),
               "model_prefix": None},
    "gemini": {"args": ("--acp",), "binaries": ("gemini",),
               "env": {"NO_BROWSER": "true", "GEMINI_FORCE_FILE_STORAGE": "true"},
               "signin_markers": (
                   "Please visit the following URL to authorize the application",
                   "Enter the authorization code",
               ),
               "model_prefix": None},
    # OpenCode's ACP server calls its OWN in-process HTTP server on loopback; with
    # the sandbox's proxy env and an empty NO_PROXY those calls went to the slot
    # proxy, were refused as malformed, and every session/new failed ("directory"
    # service) — observed in the production sandbox, not in an open container.
    # Loopback is the sandbox netns's own, so exempting it reaches nothing
    # outside. The pinned binary must never self-update (npm registry), and its
    # bundled model catalogue is used instead of models.opencode.ai; both hosts
    # are outside run egress by design (neev_provider_endpoints).
    "opencode": {"args": ("acp",), "binaries": ("opencode",),
                 "env": {"NO_PROXY": OPENCODE_LOOPBACK, "no_proxy": OPENCODE_LOOPBACK,
                         "OPENCODE_DISABLE_AUTOUPDATE": "1",
                         "OPENCODE_DISABLE_MODELS_FETCH": "1"},
                 "signin_markers": (), "model_prefix": "openai/"},
}
MAX_CONTEXT_CHARS = 64 * 1024
PROBE_TIMEOUT_SECS = 45

# Environment the agent may see. HOME is the per-user volume the launcher
# delivered; the adapter passes it on and never opens it.
ENV_ALLOWLIST = (
    "PATH", "HOME", "LANG", "LC_ALL", "LC_CTYPE", "TERM", "TZ", "TMPDIR",
    "XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_STATE_HOME", "XDG_CACHE_HOME",
    "HTTP_PROXY", "HTTPS_PROXY", "NO_PROXY", "http_proxy", "https_proxy", "no_proxy",
)

# JSON-RPC / ACP error codes (schema ErrorCode) -> fixed public reasons.
_ERROR_REASONS = {
    -32000: "provider sign-in required",
    -32601: "provider does not support request",
    -32602: "provider rejected request parameters",
    -32800: "cancelled",
}
_STOP_FAILURES = ("max_tokens", "max_turn_requests", "refusal")

_REQ_INITIALIZE, _REQ_SESSION_NEW, _REQ_PROMPT, _REQ_SET_MODEL = 1, 2, 3, 4
SIGNIN_REQUIRED = "provider sign-in required"


class AdapterError(Exception):
    """A fixed, public reason the turn could not run."""


def capabilities(provider):
    """Pure: what this adapter offers for `provider`. Printed by --describe."""
    return {
        "provider": provider,
        "protocol": "acp",
        "protocolVersion": PROTOCOL_VERSION,
        "schemaPin": SCHEMA_PIN,
        "dialect": "codex-items",
        "streamingText": True,
        "toolActivity": True,
        "reasoning": False,
        "followUp": False,
        "cancel": True,
        # Image blocks only when the agent's initialize says it takes them.
        "images": "if-agent-supports",
        "modelSelection": False,
        "effort": False,
        "speed": False,
        # stdio library tools only: HTTP tool hosts are outside sandbox egress.
        "libraryMcp": "stdio",
        "context": True,
        "probe": True,
        "modelPinnedTo": PROVIDERS[provider]["model_prefix"],
        "permissions": ["reject", "allow-once"],
    }


def _parse(argv):
    parser = argparse.ArgumentParser(prog="neev_acp_stream_adapter", add_help=True)
    parser.add_argument("--provider", required=True, choices=sorted(PROVIDERS))
    parser.add_argument("--agent", default=None, help="absolute path of the agent binary")
    parser.add_argument(
        "--allowed-agent", action="append", default=[], dest="allowed_agents",
        help="absolute path the parent allowlists; repeatable",
    )
    parser.add_argument("--cwd", default=None)
    parser.add_argument(
        "--permission-policy", choices=("reject", "allow-once"), default="reject",
        dest="permission_policy",
    )
    parser.add_argument(
        "--enable-steering", action="store_true", default=False, dest="enable_steering"
    )
    parser.add_argument("--describe", action="store_true", default=False)
    parser.add_argument("--probe", action="store_true", default=False)
    # Trusted instructions from the launcher (the response-style policy). ACP has
    # no system-prompt parameter, so it travels as a leading, delimited text block.
    parser.add_argument("--context", default=None)
    # Staged image attachments (absolute paths under --cwd), sent as ACP image
    # blocks when the agent advertises promptCapabilities.image; otherwise the
    # prompt already names them as files. Repeatable.
    parser.add_argument("--image", action="append", default=[], dest="images")
    # Shared-library MCP servers, one ACP McpServer JSON each (validated by
    # mcp_server()). Repeatable.
    parser.add_argument("--mcp-server", action="append", default=[], dest="mcp_servers")
    # Library MCP servers the server already left out (over its argv budget), by
    # name, so the trace can say so. Repeatable.
    parser.add_argument("--mcp-skipped", action="append", default=[], dest="mcp_skipped")
    parser.add_argument("prompt", nargs=argparse.REMAINDER)
    args = parser.parse_args(argv)
    prompt = list(args.prompt)
    if prompt and prompt[0] == "--":
        prompt = prompt[1:]
    args.prompt = prompt[0] if prompt else ""
    return args


def resolve_agent(provider, agent, allowed):
    """Pure-ish: the agent path, only if the parent allowlisted it. Raises."""
    if not agent or not os.path.isabs(agent):
        raise AdapterError("provider binary not allowed")
    if os.path.basename(agent) not in PROVIDERS[provider]["binaries"]:
        raise AdapterError("provider binary not allowed")
    real = os.path.realpath(agent)
    allowed_real = {os.path.realpath(p) for p in allowed if p and os.path.isabs(p)}
    if real not in allowed_real:
        raise AdapterError("provider binary not allowed")
    if not os.path.isfile(real) or not os.access(real, os.X_OK):
        raise AdapterError("provider binary not allowed")
    return agent


def agent_argv(provider, agent):
    return [agent, *PROVIDERS[provider]["args"]]


def agent_env(environ, provider=None):
    """Pure: the allowlisted environment plus the provider's FIXED headless
    switches. Requires an absolute HOME."""
    home = environ.get("HOME")
    if not home or not os.path.isabs(home):
        raise AdapterError("provider home not supplied")
    env = {k: v for k, v in environ.items() if k in ENV_ALLOWLIST and "\x00" not in v}
    if provider is not None:
        env.update(PROVIDERS[provider]["env"])
    return env


def initialize_params():
    return {
        "protocolVersion": PROTOCOL_VERSION,
        "clientCapabilities": {
            "fs": {"readTextFile": False, "writeTextFile": False},
            "terminal": False,
        },
        "clientInfo": {"name": "neev", "version": "1"},
    }


def session_new_params(cwd, mcp_servers=()):
    return {"cwd": cwd, "mcpServers": list(mcp_servers)}


MAX_IMAGES = 8
MAX_IMAGE_BYTES = 5 * 1024 * 1024
# The org library's own item limit (library.rs MAX_ITEMS_PER_ORG): every MCP
# server a library can hold is accepted, never refused for being one too many.
MAX_MCP_SERVERS = 100
# safe_mcp_name(): up to 100 name characters, "-", and an item id of up to 64.
_MCP_NAME = re.compile(r"[A-Za-z0-9_-]{1,200}\Z")
# The agent sandbox's egress admits only the provider's own endpoints
# (neev_provider_endpoints.py), so no library HTTP tool host is reachable from
# inside it. Such tools are skipped with a notice rather than forwarded to fail.
HTTP_LIBRARY_TOOLS_REACHABLE = False
NOTICE_LIBRARY_TOOL_SKIPPED = "library_tool_skipped"


def mcp_server(raw):
    """Pure: one validated ACP McpServer from JSON, or AdapterError.

    stdio: {"name","command","args"} -> {"name","command","args","env":[]}
    http:  {"name","type":"http","url"} -> {"type":"http","name","url","headers":[]}
    No env and no headers are ever forwarded: no secret travels this way."""
    try:
        value = json.loads(raw) if isinstance(raw, str) and len(raw) <= 65536 else None
    except ValueError:
        value = None
    if not isinstance(value, dict) or not isinstance(value.get("name"), str) \
            or not _MCP_NAME.match(value["name"]):
        raise AdapterError("invalid library tool")
    if value.get("type") == "http":
        if set(value) != {"name", "type", "url"}:
            raise AdapterError("invalid library tool")
        from urllib.parse import urlsplit
        try:
            url = urlsplit(value["url"]) if isinstance(value["url"], str) else None
        except ValueError:
            url = None
        if (url is None or url.scheme != "https" or not url.hostname or url.username
                or url.password or url.query or url.fragment or "\x00" in value["url"]):
            raise AdapterError("invalid library tool")
        return {"type": "http", "name": value["name"], "url": value["url"], "headers": []}
    if set(value) != {"name", "command", "args"}:
        raise AdapterError("invalid library tool")
    command, args = value["command"], value["args"]
    if (not isinstance(command, str) or not command.strip() or len(command) > 500
            or "\x00" in command or not isinstance(args, list) or len(args) > 50
            or not all(isinstance(a, str) and len(a) <= 1000 and "\x00" not in a
                       for a in args)):
        raise AdapterError("invalid library tool")
    return {"name": value["name"], "command": command, "args": list(args), "env": []}


def sniff_image(data):
    """Pure: the image MIME type from leading magic bytes, or None. The same
    classes the server stages as images (content/attachment.rs sniff_attachment);
    the file name is never consulted, so `chart` or `x.jpg.download` still works."""
    if data.startswith(b"\x89PNG\r\n\x1a\n"):
        return "image/png"
    if data.startswith(b"\xff\xd8\xff"):
        return "image/jpeg"
    if len(data) >= 12 and data.startswith(b"RIFF") and data[8:12] == b"WEBP":
        return "image/webp"
    return None


def image_block(path, cwd):
    """Pure-ish: one ACP image ContentBlock for a staged file under `cwd`, or
    None when the file is not usable as one. Only regular files inside the run
    directory (symlinks out of it are refused), bounded, whose CONTENT is a known
    image type. None is not an error: the prompt already names every staged file,
    so the agent can still open it with its own tools."""
    import base64

    if not isinstance(path, str) or not os.path.isabs(path):
        return None
    real = os.path.realpath(path)
    root = os.path.realpath(cwd)
    if not real.startswith(root + os.sep) or not os.path.isfile(real):
        return None
    try:
        size = os.path.getsize(real)
        if size <= 0 or size > MAX_IMAGE_BYTES:
            return None
        with open(real, "rb") as handle:
            data = handle.read(MAX_IMAGE_BYTES + 1)
    except OSError:
        return None
    mime = sniff_image(data)
    if mime is None or len(data) > MAX_IMAGE_BYTES:
        return None
    return {"type": "image", "mimeType": mime,
            "data": base64.b64encode(data).decode("ascii")}


def usable_mcp_servers(servers, init_result):
    """Pure: (forward, skipped_names). A library tool is forwarded only when the
    agent can load its transport AND the sandbox can reach it; every other one
    is skipped by name, so one unusable tool never costs the turn."""
    forward, skipped = [], []
    for server in servers:
        if server.get("type") == "http" and not (
                HTTP_LIBRARY_TOOLS_REACHABLE
                and agent_supports(init_result, "mcpCapabilities", "http")):
            skipped.append(server["name"])
        else:
            forward.append(server)
    return forward, skipped


def prompt_params(session_id, prompt, context=None, images=()):
    blocks = []
    if context:
        blocks.append({"type": "text",
                       "text": f"<neev-instructions>\n{context}\n</neev-instructions>"})
    blocks.append({"type": "text", "text": prompt})
    blocks.extend(images)
    return {"sessionId": session_id, "prompt": blocks}


def agent_supports(init_result, *path):
    """Pure: a boolean capability from the agent's initialize result."""
    node = init_result.get("agentCapabilities") if isinstance(init_result, dict) else None
    for key in path:
        node = node.get(key) if isinstance(node, dict) else None
    return node is True


def _option_values(options):
    """ACP select options are a flat list of {value,name} or groups of them."""
    values = []
    if not isinstance(options, list):
        return values
    for option in options:
        if not isinstance(option, dict):
            continue
        if isinstance(option.get("value"), str):
            values.append(option["value"])
        elif isinstance(option.get("options"), list):
            values.extend(v for v in _option_values(option["options"]))
    return values


def model_choice(config_options, prefix):
    """Pure: which model config change (if any) pins the session to `prefix`.

    Returns ("ok", None) when the current model already has the prefix,
    ("set", (config_id, value)) when another offered model does, or
    ("missing", None) when no offered model is from the signed-in upstream."""
    if not isinstance(config_options, list):
        return ("missing", None)
    for option in config_options:
        if not isinstance(option, dict):
            continue
        if option.get("category") != "model" and option.get("id") != "model":
            continue
        config_id = option.get("id")
        if not isinstance(config_id, str) or not config_id:
            continue
        current = option.get("currentValue")
        if isinstance(current, str) and current.startswith(prefix):
            return ("ok", None)
        for value in _option_values(option.get("options")):
            if value.startswith(prefix):
                return ("set", (config_id, value))
    return ("missing", None)


def is_signin_prompt(provider, text):
    return any(marker in text for marker in PROVIDERS[provider]["signin_markers"])


def choose_permission(options, policy):
    """Pure: a RequestPermissionOutcome. Fails closed."""
    kinds = ("allow_once",) if policy == "allow-once" else ()
    kinds += ("reject_once",)
    if isinstance(options, list):
        for kind in kinds:
            for option in options:
                if (isinstance(option, dict) and option.get("kind") == kind
                        and isinstance(option.get("optionId"), str)):
                    return {"outcome": "selected", "optionId": option["optionId"]}
    return {"outcome": "cancelled"}


def error_reason(error):
    code = error.get("code") if isinstance(error, dict) else None
    return _ERROR_REASONS.get(code, "provider refused request")


def _bounded(text, limit):
    text = text if isinstance(text, str) else ""
    return text if len(text) <= limit else text[:limit]


def flatten_tool_content(content):
    """ToolCallContent[] -> display text. Non-text blocks are named, not rendered."""
    if not isinstance(content, list):
        return ""
    parts = []
    for entry in content:
        if not isinstance(entry, dict):
            continue
        kind = entry.get("type")
        if kind == "content":
            block = entry.get("content") or {}
            if block.get("type") == "text":
                parts.append(block.get("text") or "")
            else:
                parts.append(f"[{block.get('type') or 'content'}]")
        elif kind == "diff":
            parts.append(f"[diff] {entry.get('path') or ''}".rstrip())
        elif kind == "terminal":
            parts.append("[terminal]")
    text = "\n".join(parts)
    if len(text) > MAX_TOOL_PREVIEW_CHARS:
        text = text[:MAX_TOOL_PREVIEW_CHARS] + "\n[Output preview truncated]"
    return text


class Emitter:
    def __init__(self, out):
        self._out = out
        self._lock = threading.Lock()

    def _write(self, payload):
        with self._lock:
            self._out.write(json.dumps(payload, ensure_ascii=False) + "\n")
            self._out.flush()

    def delta(self, item_id, text):
        self._write({"type": "item.delta",
                     "item": {"type": "agent_message", "id": item_id, "delta": text}})

    def message(self, item_id, text):
        self._write({"type": "item.completed",
                     "item": {"type": "agent_message", "id": item_id, "text": text}})

    def tool_started(self, item_id, command):
        self._write({"type": "item.started",
                     "item": {"type": "command_execution", "id": item_id, "command": command}})

    def tool_completed(self, item_id, command, output, failed):
        self._write({"type": "item.completed", "item": {
            "type": "command_execution", "id": item_id, "command": command,
            "aggregated_output": output, "exit_code": 1 if failed else 0}})

    def turn_completed(self):
        self._write({"type": "turn.completed"})

    def turn_failed(self, reason):
        self._write({"type": "turn.failed", "reason": reason})

    def notice(self, state, name):
        self._write({"type": "neev.notice", "state": state, "name": name})

    def account(self, status):
        self._write({"type": "neev.account", "status": status})

    def steer_rejected(self, request_id, reason):
        self._write({"type": "neev.steer", "requestId": request_id,
                     "accepted": False, "reason": reason})


class Translator:
    """`session/update` payloads -> Neev envelopes. Main thread only."""

    def __init__(self, emit):
        self._emit = emit
        self._message_id = None
        self._message_text = []
        self._message_len = 0
        self._next_message = 1
        self._tools = {}  # toolCallId -> {"command", "output"}

    def update(self, update):
        if not isinstance(update, dict):
            return
        kind = update.get("sessionUpdate")
        if kind == "agent_message_chunk":
            self._chunk(update)
        elif kind == "tool_call":
            self._tool(update, started=True)
        elif kind == "tool_call_update":
            self._tool(update, started=False)
        # agent_thought_chunk: hidden reasoning, dropped on purpose.
        # user_message_chunk, plan, usage_update, available_commands_update,
        # current_mode_update, config_option_update, session_info_update and
        # anything newer: recognised-or-unknown, carry nothing Neev renders.

    def _chunk(self, update):
        content = update.get("content") or {}
        if content.get("type") != "text":
            return
        text = content.get("text")
        if not isinstance(text, str) or not text:
            return
        message_id = update.get("messageId")
        if isinstance(message_id, str) and message_id:
            if message_id != self._message_id:
                self.close_message()
                self._message_id = message_id
        elif self._message_id is None:
            self._message_id = f"acp-msg-{self._next_message}"
            self._next_message += 1
        if self._message_len < MAX_MESSAGE_CHARS:
            self._message_text.append(text)
            self._message_len += len(text)
        self._emit.delta(self._message_id, text)

    def close_message(self):
        if self._message_id is None:
            return
        text = "".join(self._message_text)[:MAX_MESSAGE_CHARS]
        self._emit.message(self._message_id, text)
        self._message_id, self._message_text, self._message_len = None, [], 0

    def _tool(self, update, started):
        tool_id = update.get("toolCallId")
        if not isinstance(tool_id, str) or not tool_id:
            return
        # A tool breaks the prose: the next text is a new message.
        self.close_message()
        tool = self._tools.get(tool_id)
        if tool is None:
            if len(self._tools) >= MAX_OPEN_TOOLS:
                return
            title = _bounded(update.get("title"), MAX_TITLE_CHARS) or "tool"
            tool = {"command": title, "output": ""}
            self._tools[tool_id] = tool
            self._emit.tool_started(tool_id, title)
        elif isinstance(update.get("title"), str) and update["title"]:
            tool["command"] = _bounded(update["title"], MAX_TITLE_CHARS)
        if "content" in update and update["content"] is not None:
            # ACP replaces the content collection on each update.
            tool["output"] = flatten_tool_content(update["content"])
        status = update.get("status")
        if status in ("completed", "failed"):
            self._tools.pop(tool_id, None)
            self._emit.tool_completed(tool_id, tool["command"], tool["output"],
                                      status == "failed")

    def finish(self, cancelled):
        self.close_message()
        for tool_id, tool in list(self._tools.items()):
            output = tool["output"] or ("[cancelled]" if cancelled else "[unfinished]")
            self._emit.tool_completed(tool_id, tool["command"], output, True)
        self._tools.clear()


def _send(stdin, payload, lock):
    data = (json.dumps(payload, ensure_ascii=False) + "\n").encode("utf-8")
    with lock:
        stdin.write(data)
        stdin.flush()


def _reader(stream, lines):
    """Agent stdout -> queue of decoded lines. ("eof",) / ("oversized",) end it."""
    try:
        while True:
            raw = stream.readline(MAX_LINE_BYTES + 1)
            if not raw:
                break
            if len(raw) > MAX_LINE_BYTES:
                lines.put(("oversized",))
                return
            lines.put(("line", raw.decode("utf-8", errors="replace")))
    except (OSError, ValueError):
        pass
    lines.put(("eof",))


def _steer_reader(source, emit):
    """ACP v1 has no mid-turn input: every steering request is refused."""
    try:
        while True:
            line = source.readline(STEER_MAX_LINE_BYTES + 1)
            if not line:
                return
            line = line.strip()
            if not line:
                continue
            request_id = "unknown"
            try:
                record = json.loads(line) if len(line) <= STEER_MAX_LINE_BYTES else None
                if isinstance(record, dict) and isinstance(record.get("requestId"), str):
                    request_id = record["requestId"] or "unknown"
            except ValueError:
                pass
            emit.steer_rejected(request_id, FOLLOW_UP_UNSUPPORTED)
    except (OSError, ValueError):
        return


def drive(child, args, emit, cwd, stop, now=time.monotonic):
    """Run the turn against a started child. Returns an exit code."""
    lines = queue.Queue(maxsize=MAX_QUEUE_LINES)
    threading.Thread(target=_reader, args=(child.stdout, lines), daemon=True).start()
    write_lock = threading.Lock()
    translator = Translator(emit)
    session_id = None
    prompt_sent = False
    cancel_deadline = None
    handshake_deadline = now() + HANDSHAKE_TIMEOUT_SECS

    def send(payload):
        _send(child.stdin, payload, write_lock)

    def request(req_id, method, params):
        send({"jsonrpc": "2.0", "id": req_id, "method": method, "params": params})

    probe = getattr(args, "probe", False)
    prefix = PROVIDERS[args.provider]["model_prefix"]
    context = getattr(args, "context", None)
    if probe:
        handshake_deadline = now() + PROBE_TIMEOUT_SECS

    def fail(reason):
        if probe:
            if reason == SIGNIN_REQUIRED:
                emit.account("disconnected")
                return 0
            emit.account("unavailable")
            return 4
        translator.finish(cancelled=reason == "cancelled")
        emit.turn_failed(reason)
        return 4

    image_blocks = []
    forwarded_mcp = []  # what the pending session/new carries
    retried_without = []  # tools dropped by the one retry, named once it works

    def start_prompt():
        if probe:
            emit.account("supported")
            return 0
        request(_REQ_PROMPT, "session/prompt",
                prompt_params(session_id, args.prompt, context, image_blocks))
        return None

    try:
        request(_REQ_INITIALIZE, "initialize", initialize_params())
    except (BrokenPipeError, OSError):
        return fail("provider did not start")

    while True:
        if stop.is_set() and cancel_deadline is None:
            if not prompt_sent:
                return fail("cancelled")
            try:
                send({"jsonrpc": "2.0", "method": "session/cancel",
                      "params": {"sessionId": session_id}})
            except (BrokenPipeError, OSError):
                return fail("cancelled")
            cancel_deadline = now() + CANCEL_GRACE_SECS
        if cancel_deadline is not None and now() > cancel_deadline:
            return fail("cancelled")
        if not prompt_sent and now() > handshake_deadline:
            return fail("provider did not start the session")
        try:
            entry = lines.get(timeout=POLL_SECS)
        except queue.Empty:
            continue
        if entry[0] == "oversized":
            return fail("oversized provider message")
        if entry[0] == "eof":
            if cancel_deadline is not None:
                return fail("cancelled")
            if session_id is None:
                return fail("provider closed before session started")
            return fail("provider closed the stream")
        try:
            message = json.loads(entry[1])
        except ValueError:
            # stdout chatter is not protocol — unless it is the provider asking
            # for an interactive sign-in, which would otherwise block until the
            # handshake deadline. Recognised, never relayed.
            if not prompt_sent and is_signin_prompt(args.provider, entry[1]):
                return fail(SIGNIN_REQUIRED)
            continue
        if not isinstance(message, dict):
            continue

        method = message.get("method")
        if isinstance(method, str):
            params = message.get("params") if isinstance(message.get("params"), dict) else {}
            if "id" in message:
                # A request FROM the agent. Always answered, never left waiting.
                if method == "session/request_permission":
                    if cancel_deadline is not None:
                        outcome = {"outcome": "cancelled"}
                    else:
                        tool = params.get("toolCall")
                        if isinstance(tool, dict) and tool.get("title"):
                            translator.update(dict(tool, sessionUpdate="tool_call_update"))
                        outcome = choose_permission(params.get("options"),
                                                    args.permission_policy)
                    payload = {"jsonrpc": "2.0", "id": message["id"],
                               "result": {"outcome": outcome}}
                else:
                    payload = {"jsonrpc": "2.0", "id": message["id"],
                               "error": {"code": -32601, "message": "not available"}}
                try:
                    send(payload)
                except (BrokenPipeError, OSError):
                    return fail("provider closed the stream")
                continue
            if method == "session/update" and session_id is not None:
                if params.get("sessionId") == session_id:
                    translator.update(params.get("update"))
            continue  # other notifications (incl. vendor extensions) ignored

        msg_id = message.get("id")
        if "error" in message:
            if msg_id == _REQ_SET_MODEL and not prompt_sent:
                # The signed-in upstream's model could not be selected: never fall
                # back to whatever model the provider would otherwise use.
                return fail(SIGNIN_REQUIRED)
            if (msg_id == _REQ_SESSION_NEW and session_id is None and forwarded_mcp
                    and error_reason(message.get("error")) != SIGNIN_REQUIRED
                    and cancel_deadline is None):
                # The agent refused a session WITH the library tools. The chat
                # still runs: once more without them. They are named as skipped
                # only if THAT session starts — if it fails too, the tools were
                # not the problem and the turn's own failure is the story.
                retried_without.extend(server["name"] for server in forwarded_mcp)
                del forwarded_mcp[:]
                try:
                    request(_REQ_SESSION_NEW, "session/new", session_new_params(cwd, []))
                except (BrokenPipeError, OSError):
                    return fail("provider closed the stream")
                continue
            if msg_id in (_REQ_INITIALIZE, _REQ_SESSION_NEW, _REQ_PROMPT):
                if cancel_deadline is not None and msg_id == _REQ_PROMPT:
                    return fail("cancelled")
                return fail(error_reason(message.get("error")))
            continue
        result = message.get("result")
        try:
            if msg_id == _REQ_INITIALIZE and session_id is None:
                if not isinstance(result, dict) or result.get("protocolVersion") != PROTOCOL_VERSION:
                    return fail("unsupported provider protocol version")
                # Library tools the agent cannot load, or the sandbox cannot
                # reach, are skipped by name: the chat runs without them and the
                # trace says so, instead of the turn failing.
                forward, skipped = usable_mcp_servers(
                    getattr(args, "mcp_server_objects", []), result)
                for name in list(getattr(args, "mcp_skipped", [])) + skipped:
                    emit.notice(NOTICE_LIBRARY_TOOL_SKIPPED, name)
                forwarded_mcp.extend(forward)
                if getattr(args, "images", None) and agent_supports(
                        result, "promptCapabilities", "image"):
                    # Built only now, once the agent has said it takes images. A
                    # file that is not usable as one is left out: the prompt
                    # already names every staged file.
                    for path in args.images:
                        block = image_block(path, cwd)
                        if block is not None:
                            image_blocks.append(block)
                request(_REQ_SESSION_NEW, "session/new", session_new_params(cwd, forward))
            elif msg_id == _REQ_SESSION_NEW and session_id is None:
                sid = result.get("sessionId") if isinstance(result, dict) else None
                if not isinstance(sid, str) or not sid:
                    return fail("provider started no session")
                session_id = sid
                for name in retried_without:
                    emit.notice(NOTICE_LIBRARY_TOOL_SKIPPED, name)
                if prefix is not None:
                    verdict, change = model_choice(result.get("configOptions"), prefix)
                    if verdict == "missing":
                        return fail(SIGNIN_REQUIRED)
                    if verdict == "set":
                        request(_REQ_SET_MODEL, "session/set_config_option",
                                {"sessionId": session_id, "configId": change[0],
                                 "value": change[1]})
                        continue
                done = start_prompt()
                if done is not None:
                    return done
                prompt_sent = True
            elif msg_id == _REQ_SET_MODEL and session_id is not None and not prompt_sent:
                options = result.get("configOptions") if isinstance(result, dict) else None
                if model_choice(options, prefix)[0] != "ok":
                    return fail(SIGNIN_REQUIRED)
                done = start_prompt()
                if done is not None:
                    return done
                prompt_sent = True
            elif msg_id == _REQ_PROMPT and prompt_sent:
                reason = result.get("stopReason") if isinstance(result, dict) else None
                if reason == "end_turn" and cancel_deadline is None:
                    translator.finish(cancelled=False)
                    emit.turn_completed()
                    return 0
                if reason == "cancelled" or cancel_deadline is not None:
                    return fail("cancelled")
                if reason in _STOP_FAILURES:
                    return fail(reason)
                return fail("provider stopped unexpectedly")
        except (BrokenPipeError, OSError):
            return fail("provider closed the stream")


def shutdown_child(child, grace=CANCEL_GRACE_SECS):
    """Stop and reap the agent WITHOUT closing its stdout first.

    The reader thread is blocked in `child.stdout.readline()`, and closing a
    buffered reader another thread is reading waits for that read to return.
    Gemini and OpenCode exit on stdin EOF, so the read ends; Cursor's `agent acp`
    (node) does not, and the adapter hung until killed from outside — observed
    on the pinned image as a probe that printed its verdict and never exited.
    So: EOF on stdin, then terminate/kill and reap; stdout is left to the daemon
    reader (it sees EOF once the child is gone) and to process exit."""
    try:
        child.stdin.close()
    except (OSError, ValueError):
        pass
    if child.poll() is None:
        child.terminate()
    try:
        child.wait(timeout=grace)
    except subprocess.TimeoutExpired:
        child.kill()
        child.wait()


def main(argv=None, steer_input=None, environ=None):
    args = _parse(sys.argv[1:] if argv is None else argv)
    if args.describe:
        print(json.dumps(capabilities(args.provider)))
        return 0
    emit = Emitter(sys.stdout)
    if args.probe and (args.prompt or args.context or args.enable_steering):
        emit.account("unavailable")
        return 2
    if not args.probe and not args.prompt:
        emit.turn_failed("empty prompt")
        return 2
    if args.probe and (args.images or args.mcp_servers or args.mcp_skipped):
        emit.account("unavailable")
        return 2
    if args.context is not None and (not args.context or len(args.context) > MAX_CONTEXT_CHARS):
        emit.turn_failed("invalid context")
        return 2
    try:
        agent = resolve_agent(args.provider, args.agent, args.allowed_agents)
        env = agent_env(os.environ if environ is None else environ, args.provider)
    except AdapterError as error:
        if args.probe:
            emit.account("unavailable")
        else:
            emit.turn_failed(str(error))
        return 2
    cwd = args.cwd or os.getcwd()
    # Inputs are validated BEFORE any provider process exists.
    try:
        if (len(args.images) > MAX_IMAGES
                or len(args.mcp_servers) + len(args.mcp_skipped) > MAX_MCP_SERVERS):
            raise AdapterError("too many inputs")
        if not all(_MCP_NAME.match(name) for name in args.mcp_skipped):
            raise AdapterError("invalid library tool")
        args.mcp_server_objects = [mcp_server(raw) for raw in args.mcp_servers]
        names = [server["name"] for server in args.mcp_server_objects] + args.mcp_skipped
        if len(set(names)) != len(names):
            raise AdapterError("invalid library tool")
    except AdapterError as error:
        emit.turn_failed(str(error))
        return 2

    stop = threading.Event()

    def request_stop(*_unused):
        stop.set()

    previous = {sig: signal.signal(sig, request_stop) for sig in (signal.SIGTERM, signal.SIGINT)}
    try:
        try:
            child = subprocess.Popen(
                agent_argv(args.provider, agent),
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
                # stderr inherited: the launcher routes it (dev/null in prod).
                cwd=cwd,
                env=env,
            )
        except OSError:
            if args.probe:
                emit.account("unavailable")
            else:
                emit.turn_failed("provider did not start")
            return 3
        source = steer_input if steer_input is not None else (
            sys.stdin if args.enable_steering else None)
        if source is not None:
            threading.Thread(target=_steer_reader, args=(source, emit), daemon=True).start()
        try:
            return drive(child, args, emit, cwd, stop)
        finally:
            shutdown_child(child)
    finally:
        for sig, handler in previous.items():
            signal.signal(sig, handler)


if __name__ == "__main__":
    sys.exit(main())
