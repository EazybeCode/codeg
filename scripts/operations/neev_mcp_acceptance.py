#!/usr/bin/env python3
"""LINUX acceptance for shared integrations (brokered MCP). FAILS CLOSED.

Run as root INSIDE the disposable acceptance container (release kit
`accept/run-acceptance.sh`), under the candidate AppArmor profile, with the
kit's fixture mounted read-only at /opt/neev-accept:

    printf '%s\\n' "$SYN" | python3 -I neev_mcp_acceptance.py --fixture /opt/neev-accept \\
        --secret-stdin [--slot a|b] [--provider-ipv4 IP]... \\
        [--paths claude-adapter,claude-raw,codex-exec,codex-app-server] \\
        [--apparmor-host-observed]

The ONE synthetic secret (`neev-syn-...`) is read from stdin; only its sha256[:16]
is ever reported. Exactly one JSON object is printed:

    {"ok": bool, "secret_sha16": "...", "checks": {"<gate>": {"ok": bool, "detail": "..."}}}

`ok` is true only if EVERY gate passed. Every gate starts as FAIL "not run"; a
gate that cannot run or cannot be proven stays FAIL with its reason. All four
CLI paths are mandatory: one not selected with `--paths` is FAIL, never skipped.
Exit code 0 iff ok (2 on a usage error, 1 otherwise).

Gates (release-preparation.md A1-A11 / A2', python-review.md P6):
  preconditions          Linux, euid 0, Yama ptrace_scope >= 1, fixture valid per
                         `accept_fixture_spec`, pinned packages, caplaunch, slot free
                         (no broker-uid process, not quarantined), writable agents
                         root and supervisor state, network bootstrap present.
  a9_acp_no_broker       an ACP turn's adapter argv carries no brokered server.
  check_caplaunch        `--integration-check` THROUGH neev-agent-caplaunch as uid
                         65532: the `accept-stub` package is `ready`, toolCount 2.
  a1_squat_fails_closed  a lane-uid pre-bind of \\0neev-mcp/<key> in the run netns:
                         the broker exits 3 (bind=refused), `start_broker` gives no
                         handle and the agent's check reports broker_unavailable.
  a1_ready_before_agent  the broker's socket is bound in the run netns BEFORE any
                         lane process exists; the agent's FIRST connect (no retry)
                         and `initialize` succeed.
  broker_process         /proc/<broker>/status while serving: Uid/Gid = 63000+slot,
                         CapEff=CapPrm=CapBnd=CapAmb=0, NoNewPrivs 1, Umask 0077,
                         not dumpable (/proc/<pid> owned by root); the live package
                         child: same uid, no caps, NoNewPrivs 1, Umask 0077.
  a2p_a_private_tmp      the stub's marker is in the 0700 broker dir ($TMPDIR), not
                         in /tmp,/var/tmp,/dev/shm; every broker-uid entry there and
                         the marker are unreadable AS THE LANE UID (EACCES).
  a2p_b_planted_swept    broker-uid entries planted (as 6300x) before the run, and a
                         stale broker dir, are gone before the broker starts.
  a2p_c_cleanup          after a normal stop AND after SIGKILL of the launcher plus
                         the next slot acquire: nothing broker-uid left, or quarantine.
  a2p_d_no_broker_files  `find / -xdev -uid 6300x` plus every tmpfs mount: nothing.
  a2p_e_symlink          a lane-planted /tmp symlink (and one inside a lane dir) to a
                         sentinel is never followed by the sweeps; the broker's own
                         dirs are real, root/broker-owned, not group/world-writable.
  a3_uid_isolation       as the lane uid: /proc/<broker>/environ EACCES/ENOENT, the
                         broker dir unreadable, no broker process visible, the name
                         not squattable while served, the proxy refuses other hosts;
                         `secret_present` reports present + the right sha16 + broker
                         uid, and the lane's own env has no token.
  a4_leak_sweep          the kit's leak_sweep.py as root AND as the lane uid at every
                         live point (isolation, each CLI path, lifeline) and at the
                         end: 0 hits.
  a5_lifeline            a run whose broker has a LIVE package child; SIGKILL of the
                         launcher pid only: no 6300x process within 10 s; the next
                         acquire sweeps (clean) or quarantines (recorded); a live
                         stale broker-uid process makes the next acquire quarantine.
  a6_teardown            after every normal run: netns gone, broker dir gone, no
                         broker-uid process, slot not quarantined, workdir removed.
  a10_<path>             the pinned CLI inside the REAL sandbox (lane uid, run netns,
                         broker via `before_spawn`, bridge config exactly as Rust
                         emits) against a loopback fake model: PASS needs a PARSED
                         tool-use event for mcp__accept__echo whose result is
                         `echo:<marker>` with no denial/cancellation, the turn
                         completed, AND the fake model saw that result come back.
  a11_apparmor           0 new apparmor="DENIED" kernel records for this profile, read
                         from /dev/kmsg; unreadable is FAIL "not observable" unless
                         --apparmor-host-observed (the kit's host journal count, which
                         run-acceptance.sh enforces separately, is then the observer).
  no_secret_in_output    the secret is in no captured stdout/stderr/stream.

Synthetic credentials only. Nothing here reads a real secret or real user content.
"""

import argparse
import errno
import hashlib
import io
import json
import os
import platform
import re
import select
import secrets as _random
import signal
import stat
import subprocess
import sys
import threading
import time

_OPS = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, _OPS)

PATHS = ("claude-adapter", "claude-raw", "codex-exec", "codex-app-server")
PATH_GATES = {
    "claude-adapter": "a10_claude_adapter",
    "claude-raw": "a10_claude_raw",
    "codex-exec": "a10_codex_exec",
    "codex-app-server": "a10_codex_app_server",
}
GATES = (
    "preconditions",
    "a9_acp_no_broker",
    "check_caplaunch",
    "a1_squat_fails_closed",
    "a1_ready_before_agent",
    "broker_process",
    "a2p_a_private_tmp",
    "a2p_b_planted_swept",
    "a2p_c_cleanup",
    "a2p_d_no_broker_files",
    "a2p_e_symlink",
    "a3_uid_isolation",
    "a4_leak_sweep",
    "a5_lifeline",
    "a6_teardown",
) + tuple(PATH_GATES[p] for p in PATHS) + (
    "a11_apparmor",
    "no_secret_in_output",
)

TENANT = "org:999999/user:999999"
LANE_UIDS = {"a": 61000, "b": 61001}
WEB_UID = 65532
KEY = "accept"
TOOL = "mcp__%s__echo" % KEY
HOST = "neev-accept.example.com"
DEFAULT_PROVIDER_IPV4 = "160.79.104.10"
WORKSPACE_ROOT = "/data/agents"
SUPERVISOR_STATE = "/var/lib/neev-supervisor"
CAPLAUNCH = "/usr/local/bin/neev-agent-caplaunch"
SETPRIV = "/usr/bin/setpriv"
IP = "/usr/sbin/ip"
PYTHON = "/usr/bin/python3"
CLAUDE = "/usr/local/bin/claude"
CODEX = "/usr/local/bin/codex"
# Exactly what Rust emits (library_runtime.rs BRIDGE_PYTHON / BRIDGE_SCRIPT).
BRIDGE_PYTHON = "/usr/bin/python3"
BRIDGE_SCRIPT = "/opt/neev/scripts/operations/neev_mcp_bridge.py"
FAKE_MODEL = os.path.join(_OPS, "neev_mcp_fake_model.py")
FAKE_PORT = 47811
FAKE_KEY = "neev-syn-fake"
SAFE_PATH = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"
BASE_ENV = {"PATH": SAFE_PATH, "LANG": "C.UTF-8"}
SHARED_TEMP = ("/tmp", "/var/tmp", "/dev/shm")
PROMPT = "Neev acceptance: call the echo tool of the accept MCP server exactly once, then stop."
LIFELINE_SECONDS = 10.0
_TAIL = 300


class UsageError(Exception):
    pass


# --------------------------------------------------------------------------- #
# Pure: arguments, result document, secret handling
# --------------------------------------------------------------------------- #


class _Parser(argparse.ArgumentParser):
    def error(self, message):
        raise UsageError(message)


def select_paths(value):
    """Pure: the selected CLI paths in canonical order; unknown names refuse."""
    names = [part.strip() for part in (value or "").split(",") if part.strip()]
    if not names:
        raise UsageError("--paths names no path")
    unknown = [name for name in names if name not in PATHS]
    if unknown:
        raise UsageError("unknown path(s): %s" % ",".join(sorted(set(unknown))))
    return tuple(path for path in PATHS if path in names)


def unselected_path_failures(selected):
    """Pure: gate -> FAIL reason for every CLI path not selected (never a skip)."""
    return {PATH_GATES[path]: "not selected with --paths: all four CLI paths are mandatory"
            for path in PATHS if path not in selected}


def parse_args(argv):
    parser = _Parser(prog="neev_mcp_acceptance", add_help=False)
    parser.add_argument("--fixture", required=True)
    parser.add_argument("--secret-stdin", action="store_true", dest="secret_stdin")
    parser.add_argument("--slot", choices=("a", "b"), default="a")
    parser.add_argument("--provider-ipv4", action="append", dest="provider_ipv4")
    parser.add_argument("--paths", default=",".join(PATHS))
    parser.add_argument("--apparmor-host-observed", action="store_true",
                        dest="apparmor_host_observed")
    args = parser.parse_args(argv)
    if not args.secret_stdin:
        raise UsageError("--secret-stdin is required: the secret is never taken from argv or env")
    args.paths = select_paths(args.paths)
    args.provider_ipv4 = list(args.provider_ipv4 or [DEFAULT_PROVIDER_IPV4])
    return args


_SECRET_RE = re.compile(r"neev-syn-[\x21-\x7e]{16,256}\Z")


def read_secret(stream):
    """ONE synthetic secret line from `stream` (binary or text), or None."""
    line = stream.readline(4096)
    if isinstance(line, bytes):
        try:
            line = line.decode("ascii")
        except UnicodeDecodeError:
            return None
    line = line.rstrip("\r\n")
    return line if _SECRET_RE.match(line) else None


def sha16(secret):
    return hashlib.sha256(secret.encode("utf-8")).hexdigest()[:16]


def redact(text, secret):
    text = str(text)
    return text.replace(secret, "[secret]") if secret else text


class Checks:
    """Every gate starts FAIL "not run"; `ok` needs every gate set ok."""

    def __init__(self, names=GATES, secret=None):
        self.secret = secret
        self._items = {name: {"ok": False, "detail": "not run"} for name in names}

    def set(self, name, ok, detail=""):
        if name not in self._items:
            raise KeyError(name)
        self._items[name] = {"ok": ok is True, "detail": redact(detail, self.secret)[:4000]}

    def get(self, name):
        return dict(self._items[name])

    def pending(self):
        return [name for name, item in self._items.items()
                if not item["ok"] and item["detail"] == "not run"]

    def fail_pending(self, reason):
        for name in self.pending():
            self.set(name, False, reason)

    def ok(self):
        return bool(self._items) and all(item["ok"] for item in self._items.values())

    def as_dict(self):
        return {name: dict(item) for name, item in self._items.items()}


def render(checks, secret, captured=()):
    """The ONE JSON line, after asserting the secret is in nothing captured and
    not in the result itself. Returns (text, ok)."""
    if secret:
        needle = secret.encode("utf-8")
        hits = sum(1 for blob in captured if blob and needle in blob)
        checks.set("no_secret_in_output", hits == 0,
                   "%d captured outputs scanned, %d contained the synthetic secret"
                   % (len(captured), hits))
    else:
        checks.set("no_secret_in_output", False, "no synthetic secret was read")
    document = {"ok": checks.ok(), "secret_sha16": sha16(secret) if secret else None,
                "checks": checks.as_dict()}
    text = json.dumps(document, separators=(",", ":"))
    if secret and secret in text:  # last line of defence: never print it
        document = {"ok": False, "secret_sha16": sha16(secret), "checks": {
            "no_secret_in_output": {"ok": False,
                                    "detail": "the result contained the secret; suppressed"}}}
        text = json.dumps(document, separators=(",", ":"))
    return text, document["ok"]


# --------------------------------------------------------------------------- #
# Pure: stream parsers (structured events only; never substring matches)
# --------------------------------------------------------------------------- #


def _events(lines):
    for raw in lines:
        if isinstance(raw, bytes):
            raw = raw.decode("utf-8", "replace")
        raw = raw.strip()
        if not raw.startswith("{"):
            continue
        try:
            event = json.loads(raw)
        except ValueError:
            continue
        if isinstance(event, dict):
            yield event


def _texts(content):
    if isinstance(content, str):
        return [content]
    out = []
    if isinstance(content, list):
        for block in content:
            if isinstance(block, str):
                out.append(block)
            elif (isinstance(block, dict) and block.get("type") == "text"
                  and isinstance(block.get("text"), str)):
                out.append(block["text"])
    return out


_DENIAL = re.compile(r"permission|haven'?t granted|not granted|denied|reject|cancel|approv"
                     r"|not allowed|blocked", re.IGNORECASE)


def _arguments(value):
    if isinstance(value, str):
        try:
            value = json.loads(value)
        except ValueError:
            return None
    return value if isinstance(value, dict) else None


def parse_claude_stream(lines, tool, marker):
    """(ok, detail) for a Claude stream-json run: a parsed assistant `tool_use`
    naming `tool` with input {"text": marker}, its `tool_result` carrying exactly
    `echo:<marker>` (not is_error, no denial), no permission_denials for the tool,
    and a successful final `result`. Names in system/init or the prompt never count."""
    server = tool.split("__")[1] if tool.startswith("mcp__") else None
    uses, results, denials, final, server_status = [], {}, [], None, None
    for event in _events(lines):
        kind = event.get("type")
        if kind == "system" and event.get("subtype") == "init":
            for item in event.get("mcp_servers") or []:
                if isinstance(item, dict) and item.get("name") == server:
                    server_status = item.get("status")
            continue
        message = event.get("message") if isinstance(event.get("message"), dict) else {}
        content = message.get("content") if isinstance(message.get("content"), list) else []
        if kind == "assistant":
            for block in content:
                if (isinstance(block, dict) and block.get("type") == "tool_use"
                        and block.get("name") == tool):
                    uses.append((block.get("id"), block.get("input")))
        elif kind == "user":
            for block in content:
                if isinstance(block, dict) and block.get("type") == "tool_result":
                    results[block.get("tool_use_id")] = (block.get("is_error") is True,
                                                         _texts(block.get("content")))
        elif kind == "result":
            final = event
            for denial in event.get("permission_denials") or []:
                if isinstance(denial, dict) and denial.get("tool_name") == tool:
                    denials.append(denial.get("tool_use_id"))
    head = "mcp server %s status=%s; " % (server, server_status)
    if not uses:
        return False, head + "no parsed tool_use naming %s" % tool
    want = "echo:" + marker
    matched, problems = None, []
    for use_id, given in uses:
        if not isinstance(given, dict) or given.get("text") != marker:
            problems.append("tool_use %s input is not the scripted marker" % use_id)
            continue
        if use_id not in results:
            problems.append("tool_use %s has no tool_result" % use_id)
            continue
        is_error, texts = results[use_id]
        if is_error or any(_DENIAL.search(text) for text in texts):
            problems.append("tool_use %s result is an error/denial" % use_id)
            continue
        if use_id in denials:
            problems.append("tool_use %s listed in permission_denials" % use_id)
            continue
        if any(text.strip() == want for text in texts):
            matched = use_id
            break
        problems.append("tool_use %s result lacks echo:<marker>" % use_id)
    if denials:
        return False, head + "permission denial recorded for %s" % tool
    if matched is None:
        return False, head + "; ".join(problems)[:600]
    if (final is None or final.get("is_error") is True
            or str(final.get("subtype") or "").startswith("error")):
        return False, head + "tool call ok but the turn did not complete successfully"
    return True, head + "tool_use %s -> echo:<marker>; final result success" % matched


def _codex_items(lines):
    for event in _events(lines):
        kind, method = event.get("type"), event.get("method")
        if kind in ("item.started", "item.updated", "item.completed") and isinstance(
                event.get("item"), dict):
            yield kind.split(".", 1)[1], event["item"]
        elif method in ("item/started", "item/completed") and isinstance(
                event.get("params"), dict) and isinstance(event["params"].get("item"), dict):
            yield method.split("/", 1)[1], event["params"]["item"]


def _codex_turn(lines):
    completed = failed = False
    for event in _events(lines):
        kind, method = event.get("type"), event.get("method")
        if kind == "turn.completed":
            completed = True
        elif kind in ("turn.failed", "error"):
            failed = True
        elif method == "turn/completed":
            turn = (event.get("params") or {}).get("turn") or {}
            if turn.get("status") == "completed":
                completed = True
            else:
                failed = True
    return completed, failed


def parse_codex_stream(lines, server, tool, marker):
    """(ok, detail) for a Codex run (`exec --json` JSONL, the app-server adapter's
    envelopes, or raw app-server notifications): a parsed MCP tool-call item with
    server/tool, arguments {"text": marker}, status completed, no error, and a
    result whose text is exactly `echo:<marker>`; the turn completed."""
    seen, matched, problems = 0, False, []
    want = "echo:" + marker
    for phase, item in _codex_items(lines):
        if item.get("type") not in ("mcp_tool_call", "mcpToolCall"):
            continue
        if item.get("server") != server or item.get("tool") != tool:
            continue
        seen += 1
        if phase != "completed":
            continue
        status = str(item.get("status") or "").lower()
        result = item.get("result") if isinstance(item.get("result"), dict) else {}
        texts = _texts(result.get("content"))
        given = _arguments(item.get("arguments"))
        if status != "completed" or item.get("error"):
            problems.append("status=%s error=%s" % (status, bool(item.get("error"))))
        elif any(_DENIAL.search(text) for text in texts):
            problems.append("result is a denial/cancellation")
        elif not given or given.get("text") != marker:
            problems.append("arguments are not the scripted marker")
        elif not any(text.strip() == want for text in texts):
            problems.append("result lacks echo:<marker>")
        else:
            matched = True
    completed, failed = _codex_turn(lines)
    if not seen:
        return False, ("no parsed MCP tool-call item for server=%s tool=%s "
                       "(the stream carries none)" % (server, tool))
    if not matched:
        return False, "MCP tool-call item(s) seen=%d but none succeeded: %s" % (
            seen, "; ".join(problems)[:500] or "no completed item")
    if failed or not completed:
        return False, "tool call ok but the turn did not complete (failed=%s)" % failed
    return True, "mcp tool call %s/%s -> echo:<marker>; turn completed" % (server, tool)


def fake_model_summary(lines):
    summary = None
    for event in _events(lines):
        if event.get("type") == "neev.fake_model":
            summary = event
    return summary


def fake_model_ok(summary):
    if not isinstance(summary, dict):
        return False, "no fake-model summary (the model wrapper did not finish)"
    ok = (summary.get("toolCallIssued", 0) >= 1 and summary.get("toolResultSeen") is True
          and summary.get("toolResultHasMarker") is True
          and summary.get("toolResultIsError") is not True and not summary.get("timedOut"))
    detail = ("model: requests=%s issued=%s resultSeen=%s marker=%s error=%s timedOut=%s "
              "offered=%s" % (summary.get("requests"), summary.get("toolCallIssued"),
                              summary.get("toolResultSeen"), summary.get("toolResultHasMarker"),
                              summary.get("toolResultIsError"), summary.get("timedOut"),
                              ",".join(str(n) for n in (summary.get("offeredTools") or [])[:12])))
    return ok, detail


def evaluate_path(path, lines, marker):
    """(ok, detail): BOTH the CLI's parsed tool event AND the model-side evidence."""
    lines = list(lines)
    if path.startswith("claude"):
        stream_ok, stream_detail = parse_claude_stream(lines, TOOL, marker)
    else:
        stream_ok, stream_detail = parse_codex_stream(lines, KEY, "echo", marker)
    model_ok, model_detail = fake_model_ok(fake_model_summary(lines))
    return stream_ok and model_ok, "stream: %s | %s" % (stream_detail, model_detail)


def kmsg_parse(chunk):
    """Pure: (seq, message) of one /dev/kmsg record, or None."""
    if isinstance(chunk, bytes):
        chunk = chunk.decode("utf-8", "replace")
    head, sep, message = chunk.partition(";")
    fields = head.split(",")
    if not sep or len(fields) < 2:
        return None
    try:
        return int(fields[1]), message.split("\n", 1)[0]
    except ValueError:
        return None


def count_denials(records, after_seq, profile):
    """Pure: apparmor DENIED records newer than `after_seq` for `profile`."""
    want = 'profile="%s"' % profile
    return sum(1 for seq, message in records
               if seq > after_seq and 'apparmor="DENIED"' in message and want in message)


def profile_of(label):
    """Pure: (name, mode) of an AppArmor label like `name (enforce)`."""
    label = (label or "").strip("\x00\n ")
    match = re.fullmatch(r"(.+) \((\w+)\)", label)
    if match:
        return match.group(1), match.group(2)
    return (label or None), None


def bundle_for(secret):
    return json.dumps({"neevIntegrations": 1, "servers": [{
        "key": KEY, "transport": "package", "package": "accept-stub", "hosts": [HOST],
        "env": {"NEEV_ACCEPT_TOKEN": secret}}]}, separators=(",", ":")).encode("utf-8")


def claude_mcp_config():
    """The `--mcp-config` file exactly as Rust's `claude_mcp_config` emits it."""
    return {"mcpServers": {KEY: {"command": BRIDGE_PYTHON,
                                 "args": ["-I", BRIDGE_SCRIPT, KEY]}}}


def codex_mcp_overrides():
    """The `-c` overrides exactly as Rust's `codex_mcp_config` emits them."""
    return ["mcp_servers.%s.command=%s" % (KEY, json.dumps(BRIDGE_PYTHON)),
            "mcp_servers.%s.args=%s" % (KEY, json.dumps(["-I", BRIDGE_SCRIPT, KEY],
                                                         separators=(",", ":")))]


def codex_fake_config(port):
    """CODEX_HOME/config.toml pointing the pinned Codex at the loopback model.
    A file, not `-c`: `app_server_argv` refuses any `-c` outside its allowlist
    by design, and the argv must stay exactly what production builds."""
    return ('model = "neev-fake"\nmodel_provider = "neevfake"\n\n'
            '[model_providers.neevfake]\nname = "neevfake"\n'
            'base_url = "http://127.0.0.1:%d/v1"\nwire_api = "responses"\n'
            'env_key = "NEEVFAKE_API_KEY"\nrequires_openai_auth = false\n' % port)


# --------------------------------------------------------------------------- #
# Linux helpers (root, inside the acceptance container)
# --------------------------------------------------------------------------- #


def _run(argv, data=None, timeout=60, env=None):
    return subprocess.run(argv, input=data, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                          timeout=timeout, env=env or BASE_ENV, close_fds=True)


def fake_model_path(fixture, candidates=None, lstat=os.lstat):
    """The fake model beside this script, else in the kit's fixture dir; only a
    root-owned, not group/world-writable regular file. None if neither."""
    for path in candidates or (FAKE_MODEL, os.path.join(fixture, "neev_mcp_fake_model.py")):
        try:
            info = lstat(path)
        except OSError:
            continue
        if stat.S_ISREG(info.st_mode) and info.st_uid == 0 and not info.st_mode & 0o022:
            return path
    return None


def drop_argv(uid):
    return [SETPRIV, "--reuid=%d" % uid, "--regid=%d" % uid, "--clear-groups",
            "--no-new-privs", "--bounding-set=-all", "--inh-caps=-all", "--"]


def proc_status(pid):
    out = {}
    try:
        with open("/proc/%d/status" % pid, "r") as handle:
            for line in handle:
                name, _, value = line.partition(":")
                out[name.strip()] = value.strip()
    except OSError:
        pass
    return out


def proc_cmdline(pid):
    try:
        with open("/proc/%d/cmdline" % pid, "rb") as handle:
            return handle.read()
    except OSError:
        return b""


def ids_all(value, want):
    parts = (value or "").split()
    try:
        return len(parts) == 4 and all(int(part) == want for part in parts)
    except ValueError:
        return False


def caps_zero(status, names=("CapEff", "CapPrm", "CapBnd", "CapAmb")):
    try:
        return all(int(status[name], 16) == 0 for name in names)
    except (KeyError, ValueError):
        return False


def same_uid_environ_denied(pid, uid):
    """Linux, root: can a process with the SAME uid read `/proc/<pid>/environ`?

    A forked child drops to `uid` (no groups, no capabilities left after the
    uid change) and tries. True only if the kernel refuses with EACCES/EPERM —
    the property "not dumpable" exists to give. None when it cannot be measured
    (never counted as a pass)."""
    try:
        child = os.fork()
    except OSError:
        return None
    if child == 0:  # pragma: no cover - forked child, Linux root only
        code = 3
        try:
            os.setgroups([])
            os.setgid(uid)
            os.setuid(uid)
            try:
                with open("/proc/%d/environ" % pid, "rb") as handle:
                    handle.read(1)
                code = 1  # readable: NOT protected
            except PermissionError:
                code = 0  # refused: protected
            except OSError:
                code = 2  # gone or other: unknown
        finally:
            os._exit(code)
    try:
        _pid, status = os.waitpid(child, 0)
    except OSError:
        return None
    if not os.WIFEXITED(status):
        return None
    return {0: True, 1: False}.get(os.WEXITSTATUS(status))


def process_proof(pid, uid, stat=os.stat, environ_probe=same_uid_environ_denied):
    """The broker/package process facts the `broker_process` gate checks.

    "Not dumpable" is read from `/proc/<pid>/status`, which the kernel re-owns
    to root for a non-dumpable task — NOT from the `/proc/<pid>` directory,
    which on current kernels (measured: 7.0.0) keeps the task's uid even when
    PR_SET_DUMPABLE is 0. It is then proven directly: a same-uid reader must be
    refused `/proc/<pid>/environ`."""
    status = proc_status(pid)
    try:
        status_owner = stat("/proc/%d/status" % pid).st_uid
    except OSError:
        status_owner = None
    return {
        "uid": ids_all(status.get("Uid"), uid), "gid": ids_all(status.get("Gid"), uid),
        "caps_zero": caps_zero(status), "nnp": status.get("NoNewPrivs") == "1",
        "umask": status.get("Umask"), "status_owner": status_owner,
        "environ_denied_same_uid": environ_probe(pid, uid),
    }


def broker_process_ok(broker, package, cmdline_secret):
    """Pure: the `broker_process` gate. Fail-closed: every field must be proven."""
    return (broker is not None and broker["uid"] and broker["gid"] and broker["caps_zero"]
            and broker["nnp"] and broker["umask"] == "0077"
            and broker["status_owner"] == 0 and broker["environ_denied_same_uid"] is True
            and package is not None and package["uid"] and package["caps_zero"]
            and package["nnp"] and package["umask"] == "0077" and cmdline_secret is False)


def uid_processes_by_role(uid):
    import neev_agent_launch as launch

    roles = {"broker": [], "package": [], "other": []}
    for pid in launch.uid_processes(uid):
        command = proc_cmdline(pid)
        if b"neev_mcp_broker.py" in command:
            roles["broker"].append(pid)
        elif b"stub_mcp_server.py" in command:
            roles["package"].append(pid)
        else:
            roles["other"].append(pid)
    return roles


def kill_uid(uid):
    import neev_agent_launch as launch

    for pid in launch.uid_processes(uid):
        try:
            os.kill(pid, signal.SIGKILL)
        except OSError:
            pass


def wait_until(predicate, seconds, step=0.1):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        if predicate():
            return True
        time.sleep(step)
    return predicate()


_UNIX_CHECK = ("import sys\nrows=open('/proc/net/unix').read().splitlines()[1:]\n"
               "print(any(r.split()[-1:]==['@'+sys.argv[1]] for r in rows))")


def abstract_bound(netns, name):
    proc = _run([IP, "netns", "exec", netns, PYTHON, "-I", "-c", _UNIX_CHECK, name], timeout=20)
    return proc.stdout.strip() == b"True"


def mounts():
    out = []
    try:
        with open("/proc/self/mounts") as handle:
            for line in handle:
                parts = line.split()
                if len(parts) >= 3:
                    point = parts[1].encode().decode("unicode_escape")
                    out.append((point, parts[2]))
    except OSError:
        pass
    return out


def owned_entries(uids, limit=4_000_000):
    """`find / -xdev -uid U` plus every tmpfs mount (each walked without leaving
    its own device). Never follows a symlink; /proc and /sys are skipped."""
    roots = ["/"] + sorted({point for point, fstype in mounts()
                            if fstype in ("tmpfs", "devtmpfs") and point != "/"
                            and not point.startswith(("/proc", "/sys"))})
    found, visited = [], 0
    for root in roots:
        try:
            device = os.lstat(root).st_dev
        except OSError:
            continue
        stack = [root]
        while stack and visited < limit:
            directory = stack.pop()
            try:
                entries = list(os.scandir(directory))
            except OSError:
                continue
            for entry in entries:
                visited += 1
                if entry.path in ("/proc", "/sys"):
                    continue
                try:
                    info = entry.stat(follow_symlinks=False)
                except OSError:
                    continue
                if info.st_uid in uids:
                    found.append(entry.path)
                if stat.S_ISDIR(info.st_mode) and info.st_dev == device:
                    stack.append(entry.path)
    return sorted(set(found)), visited < limit


def lstat_info(path):
    try:
        info = os.lstat(path)
    except OSError as error:
        return {"exists": False, "errno": errno.errorcode.get(error.errno)}
    return {"exists": True, "dir": stat.S_ISDIR(info.st_mode), "link": stat.S_ISLNK(info.st_mode),
            "uid": info.st_uid, "mode": stat.S_IMODE(info.st_mode)}


def writable_roots(roots=SHARED_TEMP):
    """The shared temp roots that exist and are not on a read-only mount (the
    container's /var/tmp is on the read-only rootfs: nothing can write there)."""
    out = []
    for root in roots:
        try:
            if os.path.isdir(root) and not os.statvfs(root).f_flag & os.ST_RDONLY:
                out.append(root)
        except OSError:
            continue
    return out


class _Capture:
    """Stands in for `sys.stdout` while `_run_child` forwards the agent's stream:
    keeps every line and runs `on_line` on each (it may block the forwarder)."""

    def __init__(self, on_line=None):
        self.buffer = self
        self.lines = []
        self._pending = b""
        self._on_line = on_line
        self.hook_errors = []

    def write(self, data):
        if isinstance(data, str):
            data = data.encode("utf-8")
        self._pending += data
        while b"\n" in self._pending:
            line, self._pending = self._pending.split(b"\n", 1)
            self.lines.append(line)
            if self._on_line is not None:
                try:
                    self._on_line(line)
                except Exception as error:  # a hook bug must not abort the run
                    self.hook_errors.append(type(error).__name__)
        return len(data)

    def flush(self):
        pass

    def finish(self):
        if self._pending:
            self.lines.append(self._pending)
            self._pending = b""
        return self.lines


# --------------------------------------------------------------------------- #
# The acceptance run
# --------------------------------------------------------------------------- #


class Acceptance:
    def __init__(self, args, secret, checks):
        import neev_agent_launch as launch

        self.args = args
        self.slot = args.slot
        self.lane = LANE_UIDS[args.slot]
        self.broker_uid = launch.BROKER_UIDS[args.slot]
        self.secret = secret
        self.sha = sha16(secret)
        self.fixture = args.fixture
        self.checks = checks
        self.captured = []
        self.sweeps = []
        self.teardowns = []
        self.kmsg_start = None
        self.cleanup_paths = []
        self.bundle = bundle_for(secret)

    # -- bookkeeping --
    def capture(self, *blobs):
        for blob in blobs:
            if blob:
                self.captured.append(blob if isinstance(blob, bytes) else str(blob).encode())

    def tail(self, blob):
        text = blob.decode("utf-8", "replace") if isinstance(blob, bytes) else str(blob)
        return redact(text.strip()[-_TAIL:], self.secret)

    def gate(self, name, function):
        try:
            function()
        except Exception as error:  # a crash is a FAIL with its type, never a pass
            self.checks.set(name, False, "error: %s %s" % (
                type(error).__name__, redact(str(error), self.secret)[:200]))

    def integrations(self):
        import neev_agent_launch as launch

        return launch.read_integrations(io.BytesIO(self.bundle + b"\n"), accept_fixture=True)

    def make_workdir(self, tag):
        path = os.path.join(WORKSPACE_ROOT, "run-accept-%s-%s" % (tag, _random.token_hex(4)))
        os.mkdir(path, 0o700)
        os.chown(path, WEB_UID, WEB_UID)
        return path

    def spec(self, workdir, argv, provider=None):
        import neev_agent_launch as launch
        import neev_agent_sandbox as sandbox

        return sandbox.SandboxSpec(
            tenant=TENANT, slot=self.slot, workdir=workdir, run_uid=self.lane, run_gid=self.lane,
            provider_argv=tuple(argv),
            limits=sandbox.ResourceLimits(
                cpu_seconds=120, address_space_bytes=launch.address_space_bytes(provider, None),
                open_files=256, processes=256),
            egress=sandbox.EgressPolicy.provider_allowlist(self.args.provider_ipv4))

    def launcher_run(self, spec, make_env, on_line=None, start_hook=None, capture=True):
        """The launcher's own integrations run path, in this process: the slot
        proxy (lock + allowlist + integration hosts), `_refuse_stale_broker`
        (stale check + sweep), `_run_child` (broker via `before_spawn`, verified
        stop, quarantine), then `_cleanup_run_workdir`. `start_hook` wraps
        `start_broker` to OBSERVE the moment between netns wiring and spawn."""
        import neev_agent_launch as launch
        import neev_slot_proxy as slot_proxy

        integrations = self.integrations()
        out = _Capture(on_line) if capture else None
        original = launch.start_broker
        if start_hook is not None:
            launch.start_broker = start_hook(original)
        saved = sys.stdout
        rc, error = None, None
        try:
            with slot_proxy.slot_proxy("none", "integrations", spec.slot,
                                       extra_hosts=integrations.hosts) as proxy_env:
                if launch._refuse_stale_broker(slot_proxy, spec.slot):
                    rc, error = 7, "stale broker / unverified cleanup (quarantined)"
                else:
                    if out is not None:
                        sys.stdout = out
                    try:
                        rc = launch._run_child(spec, make_env(proxy_env),
                                               integrations=integrations)
                    finally:
                        sys.stdout = saved
        except slot_proxy.SlotProxyError as failure:
            rc, error = 7, "slot proxy: %s" % failure.category
        finally:
            sys.stdout = saved
            launch.start_broker = original
            signal.signal(signal.SIGTERM, signal.SIG_DFL)
            signal.signal(signal.SIGINT, signal.default_int_handler)
        removed = launch._cleanup_run_workdir(spec.workdir)
        lines = out.finish() if out is not None else []
        self.capture(*lines)
        return {"rc": rc, "error": error, "lines": lines, "workdir_removed": removed,
                "hook_errors": out.hook_errors if out is not None else []}

    def teardown_state(self, label, run):
        import neev_agent_launch as launch
        import neev_slot_proxy as slot_proxy

        state = {
            "netns_gone": not os.path.lexists("/run/netns/neev-agent-%s" % self.slot),
            "broker_dir_gone": not os.path.lexists(os.path.join(launch.BROKER_RUN_ROOT, self.slot)),
            "no_broker_process": wait_until(lambda: not launch.uid_processes(self.broker_uid), 5),
            "not_quarantined": not slot_proxy.is_quarantined(self.slot),
            "workdir_removed": run.get("workdir_removed") is True,
        }
        ok = all(state.values())
        self.teardowns.append((label, ok, state))
        return ok, state

    def caplaunch_check(self, tag):
        """The admin check THROUGH the fixed cap helper, as the web uid."""
        workdir = self.make_workdir(tag)
        argv = [SETPRIV, "--reuid=%d" % WEB_UID, "--regid=%d" % WEB_UID, "--clear-groups",
                CAPLAUNCH, "--tenant", TENANT, "--slot", self.slot, "--workdir", workdir,
                "--uid", str(self.lane), "--gid", str(self.lane), "--integration-check",
                "--integrations-stdin", "--accept-fixture"]
        for address in self.args.provider_ipv4:
            argv += ["--provider-ipv4", address]
        proc = _run(argv, data=self.bundle + b"\n", timeout=300)
        self.capture(proc.stdout, proc.stderr)
        results = {}
        for event in _events(proc.stdout.splitlines()):
            if event.get("neevIntegrationCheck") == 1 and isinstance(event.get("key"), str):
                results[event["key"]] = event
        return {"rc": proc.returncode, "results": results,
                "workdir_removed": not os.path.lexists(workdir),
                "stderr": self.tail(proc.stderr)}

    def leak_sweep(self, label):
        script = os.path.join(self.fixture, "leak_sweep.py")
        outcome = {}
        for who, prefix in (("root", []), ("lane", drop_argv(self.lane))):
            proc = _run(prefix + [PYTHON, "-I", script], data=self.secret.encode(), timeout=900)
            self.capture(proc.stdout, proc.stderr)
            try:
                result = json.loads(proc.stdout.decode("utf-8", "replace").strip().splitlines()[-1])
            except (ValueError, IndexError):
                result = {"ok": False, "error": "unparsable (rc %d)" % proc.returncode}
            outcome[who] = result
        self.sweeps.append((label, outcome))
        return outcome

    def as_uid(self, uid, code, *argv, timeout=30):
        proc = _run(drop_argv(uid) + [PYTHON, "-I", "-c", code] + list(argv), timeout=timeout)
        self.capture(proc.stdout, proc.stderr)
        return proc.returncode

    # -- gates --
    def preconditions(self):
        import neev_agent_launch as launch
        import neev_mcp_broker as broker
        import neev_slot_proxy as slot_proxy

        problems, notes = [], []
        try:
            with open("/proc/sys/kernel/yama/ptrace_scope") as handle:
                scope = int(handle.read().strip())
            if scope < 1:
                problems.append("kernel.yama.ptrace_scope=%d (< 1)" % scope)
        except (OSError, ValueError):
            problems.append("kernel.yama.ptrace_scope unreadable (Yama absent?)")
        if os.path.realpath(self.fixture) != broker.ACCEPT_FIXTURE_DIR:
            problems.append("--fixture must be %s (the broker's fixed path)"
                            % broker.ACCEPT_FIXTURE_DIR)
        if broker.accept_fixture_spec(self.fixture) is None:
            problems.append("fixture refused by accept_fixture_spec (root-owned, not "
                            "group/world-writable, no symlink)")
        if not os.path.isfile(os.path.join(self.fixture, "leak_sweep.py")):
            problems.append("fixture leak_sweep.py missing")
        for name, spec in broker.PACKAGES.items():
            if not all(os.path.isfile(part) for part in spec.argv):
                problems.append("pinned package %s missing" % name)
        for path in (broker.NODE, CAPLAUNCH, SETPRIV, IP, PYTHON, BRIDGE_SCRIPT,
                     launch.BROKER, launch.CHECKER):
            if not os.path.isfile(path):
                problems.append("missing %s" % path)
        if launch.stale_broker(self.slot):
            problems.append("a broker-uid process is alive on slot %s" % self.slot)
        if slot_proxy.is_quarantined(self.slot):
            problems.append("slot %s is quarantined" % self.slot)
        problems += self._workspace(notes)
        problems += self._network(notes)
        self.checks.set("preconditions", not problems,
                        "; ".join(problems) if problems else "ok; " + "; ".join(notes))
        return not problems

    def _workspace(self, notes):
        problems = []
        try:
            if not os.path.isdir(WORKSPACE_ROOT):
                os.makedirs(WORKSPACE_ROOT, 0o711)
                os.chown(WORKSPACE_ROOT, WEB_UID, WEB_UID)
                os.chmod(WORKSPACE_ROOT, 0o711)
                notes.append("created %s" % WORKSPACE_ROOT)
            info = os.lstat(WORKSPACE_ROOT)
            if (not stat.S_ISDIR(info.st_mode) or info.st_uid != WEB_UID
                    or info.st_mode & 0o022):
                problems.append("%s must be a %d-owned dir, not group/world-writable"
                                % (WORKSPACE_ROOT, WEB_UID))
            probe = os.path.join(WORKSPACE_ROOT, "run-accept-probe-" + _random.token_hex(4))
            os.mkdir(probe)
            os.rmdir(probe)
        except OSError as error:
            problems.append("agents root %s not writable (%s): the kit must mount a tmpfs at "
                            "/data" % (WORKSPACE_ROOT, errno.errorcode.get(error.errno)))
        try:
            locks = os.path.join(SUPERVISOR_STATE, "proxy-locks")
            os.makedirs(locks, 0o700, exist_ok=True)
            probe = os.path.join(locks, ".accept-probe")
            with open(probe, "w"):
                pass
            os.unlink(probe)
        except OSError as error:
            problems.append("%s not writable (%s): the kit must mount a tmpfs there"
                            % (SUPERVISOR_STATE, errno.errorcode.get(error.errno)))
        return problems

    def _network(self, notes):
        import neev_agent_network_bootstrap as bootstrap
        import neev_login_network_profile as profile

        bridges = [network.bridge for network in profile.network_profiles()]
        present = [os.path.exists("/sys/class/net/" + bridge) for bridge in bridges]
        proxy_netns = os.path.exists("/run/netns/" + bootstrap.PROXY_NETNS)
        if all(present) and proxy_netns:
            notes.append("network bootstrap present")
            return []
        if any(present) or proxy_netns:
            return ["network bootstrap partially present; refusing to guess"]
        try:
            names = sorted(os.listdir("/sys/class/net"))
        except OSError:
            names = []
        uplinks = [n for n in names if n == "eth0"] or [
            n for n in names if n != "lo" and not n.startswith(("nv", "veth"))]
        if not uplinks:
            return ["no uplink interface for the network bootstrap"]
        try:
            bootstrap.apply(bootstrap.plan(uplinks[0]), bootstrap.CommandRunner())
        except Exception as error:
            return ["network bootstrap failed: %s" % type(error).__name__]
        notes.append("network bootstrap applied (uplink %s)" % uplinks[0])
        return []

    def a9(self):
        import neev_agent_launch as launch

        bridge_json = json.dumps({"name": KEY, "command": BRIDGE_PYTHON,
                                  "args": ["-I", BRIDGE_SCRIPT, KEY]})
        problems = []
        for provider, pins in launch.ACP_AGENT_PINS.items():
            argv = launch.acp_stream_argv(provider, [pins[0], launch.ACP_MARKER, "--", PROMPT])
            if "--mcp-server" in argv or any(BRIDGE_SCRIPT in arg for arg in argv):
                problems.append("%s argv carries an MCP server" % provider)
        # The ACP adapter forwards no env/headers for any server it is given.
        import neev_acp_stream_adapter as acp

        if acp.mcp_server(bridge_json).get("env") != []:
            problems.append("ACP mcp_server forwards env")
        self.checks.set("a9_acp_no_broker", not problems, (
            "; ".join(problems) if problems else
            "launcher: acp_stream_argv for %s emits no MCP server when Rust passes no --mcp "
            "(ACP never gets the bridge or a bundle). Rust-side omission is proven ONLY by "
            "Rust tests (not runnable here): execution.rs "
            "acp_agents_receive_no_brokered_integration_and_no_bundle, runtime_context.rs "
            "an_acp_agent_is_never_told_it_has_brokered_tools, library_runtime.rs "
            "only_claude_and_codex_receive_brokered_integrations"
            % ",".join(sorted(launch.ACP_AGENT_PINS))))

    def check_caplaunch(self):
        result = self.caplaunch_check("ck")
        item = result["results"].get(KEY, {})
        ok = (result["rc"] == 0 and item.get("state") == "ready" and item.get("toolCount") == 2
              and result["workdir_removed"])
        state_ok, state = self.teardown_state("check_caplaunch", result)
        self.checks.set("check_caplaunch", ok and state_ok, (
            "rc=%s state=%s toolCount=%s workdir_removed=%s teardown=%s stderr=%s" % (
                result["rc"], item.get("state"), item.get("toolCount"),
                result["workdir_removed"], state, result["stderr"] if not ok else "")))

    def a1_squat(self):
        import neev_agent_launch as launch

        evidence = {}
        squatter = []
        workdir = self.make_workdir("sq")

        def hook(original):
            def start(**kw):
                netns = kw["netns"]
                code = ("import socket,sys\ns=socket.socket(socket.AF_UNIX,socket.SOCK_STREAM)\n"
                        "s.bind('\\0neev-mcp/'+sys.argv[1])\nprint('BOUND',flush=True)\n"
                        "sys.stdin.read()\n")
                proc = subprocess.Popen([IP, "netns", "exec", netns] + drop_argv(self.lane)
                                        + [PYTHON, "-I", "-c", code, KEY],
                                        stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                        stderr=subprocess.DEVNULL, start_new_session=True,
                                        close_fds=True, env=BASE_ENV)
                squatter.append(proc)
                ready, _, _ = select.select([proc.stdout], [], [], 15)
                evidence["squatter_bound"] = bool(ready) and proc.stdout.readline().strip() == b"BOUND"
                # The broker itself, directly, against the squatted name: exit 3.
                directory = launch._prepare_broker_dir(self.slot)
                env = dict(BASE_ENV)
                if kw.get("proxy"):
                    env.update({"HTTPS_PROXY": kw["proxy"], "HTTP_PROXY": kw["proxy"],
                                "NO_PROXY": ""})
                broker = subprocess.Popen(
                    launch.broker_launch_argv(netns, self.slot, TENANT, self.lane, directory,
                                              accept_fixture=True),
                    stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                    cwd=directory, env=env, start_new_session=True, close_fds=True)
                try:
                    out, err = broker.communicate(self.bundle + b"\n", timeout=30)
                except subprocess.TimeoutExpired:
                    os.killpg(broker.pid, signal.SIGKILL)
                    out, err = broker.communicate()
                self.capture(out, err)
                evidence["direct_rc"] = broker.returncode
                evidence["direct_ready_byte"] = out.startswith(b"R")
                evidence["direct_bind_refused"] = b"bind=refused" in err
                launch._remove_broker_dir(directory)
                handle = original(**kw)
                evidence["start_broker_handle"] = handle is not None
                return handle
            return start

        run = self.launcher_run(
            self.spec(workdir, (PYTHON, "-I", launch.CHECKER, KEY)),
            lambda proxy_env: launch.check_env(workdir, proxy_env), start_hook=hook)
        for proc in squatter:
            try:
                os.killpg(proc.pid, signal.SIGKILL)
            except OSError:
                pass
            proc.wait(timeout=10)
        states = [e.get("state") for e in _events(run["lines"]) if e.get("key") == KEY]
        state_ok, state = self.teardown_state("a1_squat", run)
        ok = (evidence.get("squatter_bound") is True and evidence.get("direct_rc") == 3
              and evidence.get("direct_bind_refused") is True
              and evidence.get("direct_ready_byte") is False
              and evidence.get("start_broker_handle") is False
              and states == ["broker_unavailable"] and run["rc"] == 0 and state_ok)
        self.checks.set("a1_squat_fails_closed", ok, "evidence=%s agent_state=%s rc=%s teardown=%s"
                        % (evidence, states, run["rc"], state))

    def isolation(self):
        import neev_agent_launch as launch

        U, lane, slot = self.broker_uid, self.lane, self.slot
        tag = _random.token_hex(4)
        broker_dir = os.path.join(launch.BROKER_RUN_ROOT, slot)
        # A2'(b): a previous tenant's broker-uid leftovers, created AS 6300x.
        roots = writable_roots()
        planted = ["%s/neev-accept-prev-%s" % (root, tag) for root in roots]
        planted.append("/tmp/neev-accept-prevdir-%s/" % tag)
        plant = ("import os,sys\nfor p in sys.argv[1:]:\n"
                 "    if p.endswith('/'):\n        os.mkdir(p[:-1]); open(p+'f','w').write('x')\n"
                 "    else:\n        open(p,'w').write('x')\n")
        plant_rc = self.as_uid(U, plant, *planted) if "/tmp" in roots else "no writable /tmp"
        planted = [p.rstrip("/") for p in planted]
        try:
            os.mkdir(launch.BROKER_RUN_ROOT, 0o711)
            os.chmod(launch.BROKER_RUN_ROOT, 0o711)
        except FileExistsError:
            pass
        stale_file = os.path.join(broker_dir, "stale-from-previous-run")
        if not os.path.lexists(broker_dir):
            os.mkdir(broker_dir, 0o700)
            os.chown(broker_dir, U, U)
        with open(stale_file, "w") as handle:
            handle.write("x")
        os.chown(stale_file, U, U)
        # A2'(e): a lane symlink (and one inside a lane dir) to a sentinel holding
        # a broker-uid file, outside every swept root.
        sentinel = "/run/neev-accept-sentinel-%s" % tag
        os.mkdir(sentinel, 0o755)
        victim = os.path.join(sentinel, "victim")
        with open(victim, "w") as handle:
            handle.write("x")
        os.chown(victim, U, U)
        links = ["/tmp/neev-accept-sym-%s" % tag, "/tmp/neev-accept-lanedir-%s" % tag]
        link_rc = self.as_uid(lane, "import os,sys\nos.symlink(sys.argv[1],sys.argv[2])\n"
                              "os.mkdir(sys.argv[3]); os.symlink(sys.argv[1],sys.argv[3]+'/link')",
                              sentinel, links[0], links[1])
        self.cleanup_paths += [sentinel] + links + planted

        workdir = self.make_workdir("iso")
        release = os.path.join(workdir, "neev-accept-release.json")
        marker = "neev-accept-marker-" + _random.token_hex(8)
        cfg = json.dumps({"key": KEY, "marker": marker, "release": release,
                          "env_name": "NEEV_ACCEPT_TOKEN", "broker_uid": U})
        pre, hold, result = {}, {}, {}

        def hook(original):
            def start(**kw):
                pre["planted_gone"] = [p for p in planted if os.path.lexists(p)] == []
                pre["stale_dir_file_gone"] = not os.path.lexists(stale_file)
                pre["lane_processes_before_broker"] = len(launch.uid_processes(lane))
                handle = original(**kw)
                pre["broker_started"] = handle is not None
                pre["socket_bound_before_spawn"] = bool(
                    handle is not None and abstract_bound(kw["netns"], "neev-mcp/" + KEY))
                pre["lane_processes_before_spawn"] = len(launch.uid_processes(lane))
                return handle
            return start

        def on_line(line):
            if line.startswith(b"PROBE-RESULT "):
                result.update(json.loads(line[len(b"PROBE-RESULT "):]))
                return
            if not line.startswith(b"PROBE-HOLD "):
                return
            probe = json.loads(line[len(b"PROBE-HOLD "):])
            hold["probe"] = probe
            roles = uid_processes_by_role(U)
            hold["roles"] = {k: len(v) for k, v in roles.items()}
            hold["broker"] = process_proof(roles["broker"][0], U) if len(roles["broker"]) == 1 else None
            hold["package"] = process_proof(roles["package"][0], U) if roles["package"] else None
            hold["cmdline_secret"] = any(self.secret.encode() in proc_cmdline(pid)
                                         for pid in roles["broker"] + roles["package"])
            name = os.path.basename(str((probe.get("secret_present") or {}).get("tmp_marker") or ""))
            hold["marker_in_broker_dir"] = bool(name) and os.path.exists(os.path.join(broker_dir, name))
            hold["marker_in_shared"] = bool(name) and any(
                os.path.lexists(os.path.join(root, name)) for root in SHARED_TEMP)
            hold["shared_entries"] = launch.leftover_uid_temp(U)
            hold["run_root"] = lstat_info(launch.BROKER_RUN_ROOT)
            hold["broker_dir"] = lstat_info(broker_dir)
            hold["stale_in_new_dir"] = os.path.lexists(stale_file)
            self.leak_sweep("isolation-hold")
            targets = [[broker_dir, "list"]]
            if name:
                targets.append([os.path.join(broker_dir, name), "read"])
            targets += [["/proc/%d/environ" % pid, "read"]
                        for pid in roles["broker"] + roles["package"]]
            targets += [[path, "read"] for path in hold["shared_entries"]]
            hold["targets"] = len(targets)
            temp = release + ".tmp"
            with open(temp, "w") as handle:
                json.dump({"targets": targets}, handle)
            os.chmod(temp, 0o644)
            os.rename(temp, release)

        run = self.launcher_run(
            self.spec(workdir, (PYTHON, "-I", "-c", ISOLATION_PROBE, cfg)),
            lambda proxy_env: launch.check_env(workdir, proxy_env),
            on_line=on_line, start_hook=hook)
        state_ok, state = self.teardown_state("isolation", run)
        probe = hold.get("probe") or {}
        access = {path: outcome for path, _kind, outcome in result.get("access") or []}

        # A1 ready-before-agent
        ok = (pre.get("broker_started") is True and pre.get("socket_bound_before_spawn") is True
              and pre.get("lane_processes_before_spawn") == 0
              and probe.get("first_connect") == "ok" and probe.get("initialize") == "ok"
              and probe.get("tools") == ["echo", "secret_present"] and run["rc"] == 0)
        self.checks.set("a1_ready_before_agent", ok, "pre=%s first_connect=%s initialize=%s "
                        "tools=%s rc=%s" % (pre, probe.get("first_connect"), probe.get("initialize"),
                                            probe.get("tools"), run["rc"]))
        # Broker process proof
        b, p = hold.get("broker"), hold.get("package")
        ok = broker_process_ok(b, p, hold.get("cmdline_secret"))
        self.checks.set("broker_process", ok, "roles=%s broker=%s package=%s cmdline_secret=%s "
                        "(not dumpable == /proc/<pid>/status owned by 0 AND same-uid environ "
                        "read refused)" % (hold.get("roles"), b, p, hold.get("cmdline_secret")))
        # A2'(a)
        shared = hold.get("shared_entries") or []
        marker_path = None
        name = os.path.basename(str((probe.get("secret_present") or {}).get("tmp_marker") or ""))
        if name:
            marker_path = os.path.join(broker_dir, name)
        bd = hold.get("broker_dir") or {}
        ok = (hold.get("marker_in_broker_dir") is True and hold.get("marker_in_shared") is False
              and bd.get("dir") is True and bd.get("link") is False and bd.get("uid") == U
              and bd.get("mode") == 0o700 and result.get("released") is True
              and marker_path is not None and access.get(marker_path) == "EACCES"
              and all(access.get(path) == "EACCES" for path in shared)
              and p is not None and p["umask"] == "0077")
        self.checks.set("a2p_a_private_tmp", ok, (
            "marker in broker dir=%s in shared=%s; broker_dir=%s; lane access to marker=%s; "
            "broker-uid shared entries during run=%d lane access=%s; package umask=%s"
            % (hold.get("marker_in_broker_dir"), hold.get("marker_in_shared"), bd,
               access.get(marker_path), len(shared),
               sorted(set(access.get(path) for path in shared)), p and p["umask"])))
        # A2'(b)
        ok = (plant_rc == 0 and pre.get("planted_gone") is True
              and pre.get("stale_dir_file_gone") is True and hold.get("stale_in_new_dir") is False)
        self.checks.set("a2p_b_planted_swept", ok, "planted_as_6300x in %s rc=%s gone_before_"
                        "start=%s stale_broker_dir_file_gone=%s (read-only roots skipped: %s)"
                        % (roots, plant_rc, pre.get("planted_gone"), pre.get("stale_dir_file_gone"),
                           sorted(set(SHARED_TEMP) - set(roots))))
        # A2'(c) normal stop (SIGKILL half: a5)
        self.normal_cleanup = (not launch.leftover_uid_temp(U) and state["broker_dir_gone"]
                               and state["no_broker_process"])
        # A2'(e)
        rr = hold.get("run_root") or {}
        ok = (link_rc == 0 and os.path.exists(victim) and os.path.islink(links[0])
              and os.path.islink(links[1] + "/link") and rr.get("dir") is True
              and rr.get("link") is False and rr.get("uid") == 0
              and isinstance(rr.get("mode"), int) and rr["mode"] & 0o022 == 0
              and bd.get("link") is False)
        self.checks.set("a2p_e_symlink", ok, "lane symlinks planted rc=%s; sentinel victim "
                        "survived every sweep=%s; /run/neev-mcp=%s; broker dir=%s"
                        % (link_rc, os.path.exists(victim), rr, bd))
        # A3
        sp = probe.get("secret_present") or {}
        proc_targets = [path for path in access if path.startswith("/proc/")]
        environ_ok = all(access[path] in ("EACCES", "ENOENT", "EPERM") for path in proc_targets)
        ok = (environ_ok and proc_targets != [] and access.get(broker_dir) == "EACCES"
              and sp.get("present") is True and sp.get("sha16") == self.sha
              and sp.get("uid") == U and probe.get("token_in_env") is False
              and result.get("broker_processes_visible") == 0
              and result.get("squat_while_running") == "EADDRINUSE"
              and not str(result.get("proxy_other_host") or "").startswith(("HTTP/1.1 200",
                                                                             "HTTP/1.0 200"))
              and hold.get("cmdline_secret") is False)
        self.checks.set("a3_uid_isolation", ok, (
            "lane: environ=%s broker_dir=%s broker_procs_visible=%s squat=%s proxy_other=%s; "
            "secret_present present=%s sha16_match=%s uid=%s; lane env has token=%s"
            % (sorted(set(access.get(path) for path in proc_targets)), access.get(broker_dir),
               result.get("broker_processes_visible"), result.get("squat_while_running"),
               result.get("proxy_other_host"), sp.get("present"), sp.get("sha16") == self.sha,
               sp.get("uid"), probe.get("token_in_env"))))
        # A2'(d) after this run's teardown (plants removed first: the victim is ours)
        self.remove_plants()
        self.find_after_isolation = owned_entries({U})

    def remove_plants(self):
        import shutil

        for path in self.cleanup_paths:
            try:
                if os.path.islink(path) or not os.path.isdir(path):
                    os.unlink(path)
                else:
                    shutil.rmtree(path)
            except OSError:
                pass
        self.cleanup_paths = []

    def cli_path(self, path):
        import neev_agent_launch as launch

        gate = PATH_GATES[path]
        provider = "claude" if path.startswith("claude") else "codex"
        binary = CLAUDE if provider == "claude" else CODEX
        model = fake_model_path(self.fixture)
        missing = [p for p in (binary, launch.CLAUDE_ADAPTER if path == "claude-adapter"
                               else None, launch.ADAPTER if path == "codex-app-server" else None)
                   if p and not os.path.isfile(p)]
        if model is None:
            missing.append("neev_mcp_fake_model.py (root-owned, beside this script or in the "
                           "fixture)")
        if missing:
            self.checks.set(gate, False, "unprovable before first real use: missing %s"
                            % ",".join(missing))
            return
        workdir = self.make_workdir(path.replace("-", "")[:12])
        home = os.path.join(workdir, "home")
        codex_home = os.path.join(home, ".codex")
        os.makedirs(codex_home, 0o700)
        with open(os.path.join(codex_home, "config.toml"), "w") as handle:
            handle.write(codex_fake_config(FAKE_PORT))
        for directory, _dirs, files in os.walk(home):
            os.chown(directory, self.lane, self.lane)
            for name in files:
                os.chown(os.path.join(directory, name), self.lane, self.lane)
        library = os.path.join(workdir, "neev-library")
        os.mkdir(library, 0o755)
        config = os.path.join(library, "mcp.json")
        with open(config, "w") as handle:
            json.dump(claude_mcp_config(), handle, indent=2)
        os.chmod(config, 0o644)
        os.chown(library, WEB_UID, WEB_UID)
        os.chown(config, WEB_UID, WEB_UID)
        style = launch.load_response_style()
        if provider == "claude":
            raw = [binary, "-p", PROMPT, "--output-format", "stream-json", "--verbose",
                   "--mcp-config", config, "--allowedTools", "mcp__" + KEY]
            raw = launch.response_style_argv("claude", raw, style)
            if path == "claude-adapter":
                cli = launch.claude_stream_argv("claude", raw, enable_steering=True)
                if cli[:3] != [launch.sandbox.PYTHON, "-I", launch.CLAUDE_ADAPTER]:
                    self.checks.set(gate, False, "the launcher did not route to the adapter")
                    launch._cleanup_run_workdir(workdir)
                    return
            else:
                cli = launch.externally_sandboxed_argv("claude", raw)
        else:
            raw = [binary, "exec", "--json", PROMPT]
            for override in codex_mcp_overrides():
                raw += ["-c", override]
            raw += ["--skip-git-repo-check"]
            raw = launch.response_style_argv("codex", raw, style)
            if path == "codex-exec":
                cli = launch.externally_sandboxed_argv("codex", raw)
            else:
                cli = launch.app_server_argv("codex", raw, enable_steering=True)
        marker = "neev-accept-marker-" + _random.token_hex(8)
        argv = [PYTHON, "-I", model, "--port", str(FAKE_PORT), "--marker", marker,
                "--tool", TOOL, "--timeout", "240", "--"] + list(cli)

        def make_env(proxy_env):
            env = {"PATH": launch._DEFAULT_PATH, "LANG": "C.UTF-8", "HOME": home}
            env.update(proxy_env)  # HTTPS_PROXY stays: the broker reads it from here
            loop = "127.0.0.1,localhost"
            env.update({"NO_PROXY": loop, "no_proxy": loop,
                        "ANTHROPIC_BASE_URL": "http://127.0.0.1:%d" % FAKE_PORT,
                        "ANTHROPIC_API_KEY": FAKE_KEY, "NEEVFAKE_API_KEY": FAKE_KEY,
                        "CODEX_HOME": codex_home, "DISABLE_AUTOUPDATER": "1",
                        "DISABLE_TELEMETRY": "1", "DISABLE_ERROR_REPORTING": "1",
                        "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC": "1"})
            return env

        swept = []

        def on_line(line):
            if not swept and line.strip().startswith(b"{"):
                swept.append(True)
                self.leak_sweep("cli-" + path)

        run = self.launcher_run(self.spec(workdir, argv, provider), make_env, on_line=on_line)
        state_ok, state = self.teardown_state("cli-" + path, run)
        ok, detail = evaluate_path(path, run["lines"], marker)
        self.checks.set(gate, ok and run["rc"] == 0 and state_ok, "%s; rc=%s teardown=%s%s" % (
            detail, run["rc"], "ok" if state_ok else state,
            "" if ok else "; stream tail=" + self.tail(b"\n".join(run["lines"][-3:]))))

    def lifeline(self):
        import neev_agent_egress as egress
        import neev_agent_launch as launch
        import neev_slot_proxy as slot_proxy

        U, slot = self.broker_uid, self.slot
        evidence = {}
        workdir = self.make_workdir("life")
        cfg = {"slot": slot, "lane": self.lane, "workdir": workdir,
               "provider_ipv4": self.args.provider_ipv4, "fixture": self.fixture}
        holder = subprocess.Popen(
            [PYTHON, "-I", os.path.abspath(__file__), "--internal-holder", json.dumps(cfg)],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
            start_new_session=True, close_fds=True, env=BASE_ENV)
        errors = []
        drain = threading.Thread(target=lambda: errors.append(holder.stderr.read()), daemon=True)
        drain.start()
        holder.stdin.write(self.bundle + b"\n")
        holder.stdin.close()
        live = None
        deadline = time.monotonic() + 180
        while time.monotonic() < deadline and live is None:
            ready, _, _ = select.select([holder.stdout], [], [], 1)
            if ready:
                line = holder.stdout.readline()
                if not line:
                    break
                self.capture(line)
                if line.startswith(b"LIVE "):
                    live = json.loads(line[5:])
        roles = uid_processes_by_role(U)
        evidence["live_echo_ok"] = bool(live) and str(live.get("echo") or "").startswith("echo:")
        evidence["live_broker"] = len(roles["broker"])
        evidence["live_package_child"] = len(roles["package"])
        if live is not None:
            self.leak_sweep("lifeline-live")
        os.kill(holder.pid, signal.SIGKILL)  # the launcher pid ONLY
        holder.wait(timeout=10)
        started = time.monotonic()
        gone = wait_until(lambda: not launch.uid_processes(U), LIFELINE_SECONDS)
        evidence["broker_gone_within_10s"] = gone
        evidence["broker_gone_after_s"] = round(time.monotonic() - started, 2)
        drain.join(5)
        self.capture(*errors)
        # What the dead launcher left (recorded), then the cleanup a supervisor does
        # for the launcher's OTHER children — NOT the broker's leftovers.
        proxy_ip = slot_proxy.proxy_bind_ip(slot).encode()
        proxies = [pid for pid in (int(p) for p in os.listdir("/proc") if p.isdigit())
                   if b"neev_slot_proxy.py" in proc_cmdline(pid)
                   and proxy_ip in proc_cmdline(pid)]
        evidence["orphans"] = {
            "lane_processes": len(launch.uid_processes(self.lane)), "slot_proxy": len(proxies),
            "netns": os.path.lexists("/run/netns/neev-agent-%s" % slot),
            "broker_dir": os.path.lexists(os.path.join(launch.BROKER_RUN_ROOT, slot)),
            "broker_uid_temp": len(launch.leftover_uid_temp(U))}
        kill_uid(self.lane)
        for pid in proxies:
            try:
                os.kill(pid, signal.SIGKILL)
            except OSError:
                pass
        wait_until(lambda: not launch.uid_processes(self.lane), 5)
        plan = egress.plan_provider_network(TENANT, slot, self.args.provider_ipv4)
        for command in egress.teardown_commands(plan):
            _run(command, timeout=15)
        launch._cleanup_run_workdir(workdir)
        # A killed broker's leftovers, planted as the broker uid.
        orphan = "/tmp/neev-accept-orphan-%s" % _random.token_hex(4)
        self.as_uid(U, "import sys; open(sys.argv[1],'w').write('x')", orphan)
        nxt = self.caplaunch_check("next")
        quarantined = slot_proxy.is_quarantined(slot)
        item = nxt["results"].get(KEY, {})
        clean = (not launch.leftover_uid_temp(U) and not os.path.lexists(orphan)
                 and not os.path.lexists(os.path.join(launch.BROKER_RUN_ROOT, slot))
                 and not launch.uid_processes(U))
        if nxt["rc"] == 0 and item.get("state") == "ready" and clean and not quarantined:
            outcome = "sweep"
        elif nxt["rc"] == 7 and quarantined:
            outcome = "quarantine"
        else:
            outcome = "neither (rc=%s state=%s clean=%s quarantined=%s stderr=%s)" % (
                nxt["rc"], item.get("state"), clean, quarantined, nxt["stderr"])
        evidence["next_acquire"] = outcome
        if outcome == "quarantine":
            evidence["recovered"] = self.recover()
        # B2: a LIVE stale broker-uid process makes the next acquire quarantine.
        stale = subprocess.Popen(drop_argv(U) + [PYTHON, "-I", "-c", "import time; time.sleep(180)"],
                                 stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                                 stderr=subprocess.DEVNULL, start_new_session=True, env=BASE_ENV)
        wait_until(lambda: bool(launch.uid_processes(U)), 10)
        refused = self.caplaunch_check("stale")
        evidence["stale_broker"] = "rc=%s quarantined=%s" % (refused["rc"],
                                                             slot_proxy.is_quarantined(slot))
        stale_ok = refused["rc"] == 7 and slot_proxy.is_quarantined(slot)
        try:
            os.killpg(stale.pid, signal.SIGKILL)
        except OSError:
            pass
        stale.wait(timeout=10)
        evidence["stale_recovered"] = self.recover()
        ok = (evidence["live_echo_ok"] and evidence["live_broker"] == 1
              and evidence["live_package_child"] >= 1 and gone
              and outcome in ("sweep", "quarantine") and stale_ok
              and evidence["stale_recovered"] is True)
        self.checks.set("a5_lifeline", ok, str(evidence))
        self.sigkill_cleanup = outcome in ("sweep", "quarantine")
        self.sigkill_detail = outcome

    def recover(self):
        """Operator recovery in the disposable container: only after the launcher's
        own reclaim verifies the slot clean is the quarantine lifted."""
        import neev_agent_launch as launch
        import neev_slot_proxy as slot_proxy

        if not wait_until(lambda: not launch.uid_processes(self.broker_uid), 10):
            return False
        if not launch.reclaim_slot(self.slot):
            return False
        slot_proxy.clear_quarantine(self.slot)
        return not slot_proxy.is_quarantined(self.slot)

    def finish(self):
        U = self.broker_uid
        found, complete = owned_entries({U})
        iso_found, iso_complete = getattr(self, "find_after_isolation", (None, False))
        ok = (iso_found == [] and iso_complete and found == [] and complete)
        self.checks.set("a2p_d_no_broker_files", ok, (
            "after isolation teardown: %s; at the end: %s (walk complete=%s/%s)" % (
                "none" if iso_found == [] else iso_found, "none" if found == [] else found[:10],
                iso_complete, complete)))
        normal = getattr(self, "normal_cleanup", None)
        sigkill = getattr(self, "sigkill_cleanup", None)
        self.checks.set("a2p_c_cleanup", normal is True and sigkill is True,
                        "normal stop clean=%s; after SIGKILL + next acquire=%s"
                        % (normal, getattr(self, "sigkill_detail", "not run")))
        self.leak_sweep("final")
        labels = [label for label, _ in self.sweeps]
        required = {"isolation-hold", "lifeline-live", "final"}
        hits = {label: {who: (r.get("ok"), r.get("hits")) for who, r in outcome.items()}
                for label, outcome in self.sweeps}
        ok = required <= set(labels) and all(
            r.get("ok") is True for _, outcome in self.sweeps for r in outcome.values())
        self.checks.set("a4_leak_sweep", ok, "points=%s missing=%s results=%s" % (
            labels, sorted(required - set(labels)), hits))
        ok = bool(self.teardowns) and all(item_ok for _, item_ok, _ in self.teardowns) and any(
            label == "isolation" for label, _, _ in self.teardowns)
        self.checks.set("a6_teardown", ok, "; ".join(
            "%s=%s" % (label, "ok" if item_ok else state) for label, item_ok, state in self.teardowns))

    # -- A11 --
    def kmsg_records(self):
        fd = os.open("/dev/kmsg", os.O_RDONLY | os.O_NONBLOCK)
        records = []
        try:
            while True:
                try:
                    chunk = os.read(fd, 16384)
                except BlockingIOError:
                    break
                except OSError as error:
                    if error.errno == errno.EPIPE:  # overwritten records: keep reading
                        continue
                    raise
                if not chunk:
                    break
                record = kmsg_parse(chunk)
                if record is not None:
                    records.append(record)
        finally:
            os.close(fd)
        return records

    def kmsg_begin(self):
        try:
            records = self.kmsg_records()
            self.kmsg_start = max([seq for seq, _ in records] or [0])
        except OSError as error:
            self.kmsg_start = None
            self.kmsg_error = errno.errorcode.get(error.errno, str(error.errno))

    def a11(self):
        label = None
        for path in ("/proc/self/attr/apparmor/current", "/proc/self/attr/current"):
            try:
                with open(path) as handle:
                    label = handle.read()
                break
            except OSError:
                continue
        profile, mode = profile_of(label)
        if not profile or profile == "unconfined" or mode != "enforce":
            self.checks.set("a11_apparmor", False, "not confined by an enforcing AppArmor "
                            "profile (label=%r)" % (label or "").strip("\x00\n "))
            return
        if self.kmsg_start is not None:
            try:
                denied = count_denials(self.kmsg_records(), self.kmsg_start, profile)
            except OSError as error:
                denied, why = None, errno.errorcode.get(error.errno)
            if denied is not None:
                self.checks.set("a11_apparmor", denied == 0, "%d new DENIED records for %s "
                                "(from /dev/kmsg)" % (denied, profile))
                return
        else:
            why = getattr(self, "kmsg_error", "unknown")
        if self.args.apparmor_host_observed:
            self.checks.set("a11_apparmor", True, (
                "kernel log not readable in-container (%s); delegated to the host observer: "
                "run-acceptance.sh requires 0 apparmor=\"DENIED\" for profile=\"%s\" in "
                "journalctl -k over this run" % (why, profile)))
        else:
            self.checks.set("a11_apparmor", False, "not observable: /dev/kmsg %s (pass "
                            "--apparmor-host-observed only when the kit counts host denials)"
                            % why)


ISOLATION_PROBE = r'''
import errno, json, os, socket, sys, time
cfg = json.loads(sys.argv[1])
out = {"uid": os.getuid()}
def err(e):
    return errno.errorcode.get(e.errno, str(e.errno))
def emit(tag):
    print(tag + " " + json.dumps(out), flush=True)
def rpc(s, f, m):
    s.sendall((json.dumps(m) + "\n").encode())
    while True:
        line = f.readline()
        if not line:
            raise EOFError("closed")
        r = json.loads(line)
        if isinstance(r, dict) and r.get("id") == m["id"]:
            return r
def text_of(r):
    try:
        return r["result"]["content"][0]["text"]
    except Exception:
        return None
addr = "\0neev-mcp/" + cfg["key"]
s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
try:
    s.connect(addr)  # ONE attempt: the broker must already be bound
    out["first_connect"] = "ok"
except OSError as e:
    out["first_connect"] = err(e)
    emit("PROBE-HOLD")
    emit("PROBE-RESULT")
    sys.exit(0)
s.settimeout(60)
f = s.makefile("rb")
try:
    init = rpc(s, f, {"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
        "protocolVersion": "2025-06-18", "capabilities": {},
        "clientInfo": {"name": "neev-accept-probe", "version": "1"}}})
    out["initialize"] = "ok" if "result" in init else "error"
    s.sendall(b'{"jsonrpc":"2.0","method":"notifications/initialized"}\n')
    tools = rpc(s, f, {"jsonrpc": "2.0", "id": 2, "method": "tools/list"})
    out["tools"] = sorted(t.get("name") for t in (tools.get("result") or {}).get("tools") or []
                          if isinstance(t, dict))
    sp = text_of(rpc(s, f, {"jsonrpc": "2.0", "id": 3, "method": "tools/call",
                            "params": {"name": "secret_present", "arguments": {}}}))
    out["secret_present"] = json.loads(sp) if sp else None
    out["echo"] = text_of(rpc(s, f, {"jsonrpc": "2.0", "id": 4, "method": "tools/call",
                                     "params": {"name": "echo",
                                                "arguments": {"text": cfg["marker"]}}}))
except Exception as e:
    out["rpc_error"] = type(e).__name__
out["token_in_env"] = cfg["env_name"] in os.environ
emit("PROBE-HOLD")
deadline = time.time() + 300
while not os.path.exists(cfg["release"]) and time.time() < deadline:
    time.sleep(0.2)
try:
    with open(cfg["release"]) as h:
        rel = json.load(h)
except Exception:
    rel = {}
out["released"] = bool(rel)
access = []
for path, kind in rel.get("targets", []):
    try:
        if kind == "list":
            os.listdir(path)
        else:
            with open(path, "rb") as h:
                h.read(1)
        access.append([path, kind, "readable"])
    except OSError as e:
        access.append([path, kind, err(e)])
out["access"] = access
t = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
try:
    t.bind(addr)
    out["squat_while_running"] = "bound"
except OSError as e:
    out["squat_while_running"] = err(e)
finally:
    t.close()
visible = 0
for pid in os.listdir("/proc"):
    if pid.isdigit():
        try:
            with open("/proc/%s/status" % pid) as h:
                for line in h:
                    if line.startswith("Uid:") and int(line.split()[1]) == cfg["broker_uid"]:
                        visible += 1
        except (OSError, ValueError):
            pass
out["broker_processes_visible"] = visible
try:
    host, port = os.environ["HTTPS_PROXY"].split("//", 1)[1].rstrip("/").rsplit(":", 1)
    p = socket.create_connection((host, int(port)), timeout=15)
    p.sendall(b"CONNECT example.com:443 HTTP/1.1\r\nHost: example.com:443\r\n\r\n")
    out["proxy_other_host"] = p.recv(64).split(b"\r\n", 1)[0].decode("latin-1")[:40] or "closed"
    p.close()
except Exception as e:
    out["proxy_other_host"] = "error:" + type(e).__name__
s.close()
emit("PROBE-RESULT")
'''

LIFE_PROBE = r'''
import json, socket, sys, time
cfg = json.loads(sys.argv[1])
deadline = time.time() + 20
while True:
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    try:
        s.connect("\0neev-mcp/" + cfg["key"])
        break
    except OSError:
        s.close()
        if time.time() > deadline:
            print("LIVE " + json.dumps({"echo": None}), flush=True)
            sys.exit(0)
        time.sleep(0.2)
f = s.makefile("rb")
def rpc(m):
    s.sendall((json.dumps(m) + "\n").encode())
    while True:
        r = json.loads(f.readline())
        if r.get("id") == m["id"]:
            return r
rpc({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
    "protocolVersion": "2025-06-18", "capabilities": {},
    "clientInfo": {"name": "neev-accept-life", "version": "1"}}})
s.sendall(b'{"jsonrpc":"2.0","method":"notifications/initialized"}\n')
r = rpc({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
         "params": {"name": "echo", "arguments": {"text": "neev-accept-lifeline"}}})
try:
    text = r["result"]["content"][0]["text"]
except Exception:
    text = None
print("LIVE " + json.dumps({"echo": text}), flush=True)
time.sleep(300)  # holds the connection: the package child stays alive
'''


def holder_main(argv):  # pragma: no cover - Linux, spawned by `lifeline`
    """A launcher stand-in for the lifeline gate: the launcher's own run path
    (slot proxy, stale refusal, `_run_child` with the broker lifeline pipe) with
    a lane probe that keeps a package child alive. The parent SIGKILLs THIS pid."""
    import neev_agent_launch as launch

    cfg = json.loads(argv[0])
    os.environ["NEEV_AGENT_WORKSPACE_ROOT"] = WORKSPACE_ROOT
    line = sys.stdin.buffer.readline(1024 * 1024)
    secret = None
    try:
        secret = json.loads(line)["servers"][0]["env"]["NEEV_ACCEPT_TOKEN"]
    except (ValueError, KeyError, IndexError, TypeError):
        return 2
    args = argparse.Namespace(slot=cfg["slot"], provider_ipv4=cfg["provider_ipv4"],
                              fixture=cfg["fixture"])
    acceptance = Acceptance(args, secret, Checks(secret=secret))
    acceptance.lane = cfg["lane"]
    workdir = cfg["workdir"]
    probe_cfg = json.dumps({"key": KEY})
    acceptance.launcher_run(
        acceptance.spec(workdir, (PYTHON, "-I", "-c", LIFE_PROBE, probe_cfg)),
        lambda proxy_env: launch.check_env(workdir, proxy_env), capture=False)
    return 0


def run_linux(args, secret, checks):  # pragma: no cover - Linux, root
    os.environ["NEEV_AGENT_WORKSPACE_ROOT"] = WORKSPACE_ROOT
    acceptance = Acceptance(args, secret, checks)
    acceptance.kmsg_begin()
    try:
        ready = False
        try:
            ready = acceptance.preconditions()
        except Exception as error:
            checks.set("preconditions", False, "error: %s" % type(error).__name__)
        if not ready:
            checks.fail_pending("not run: preconditions failed")
            return acceptance.captured
        acceptance.gate("a9_acp_no_broker", acceptance.a9)
        acceptance.gate("check_caplaunch", acceptance.check_caplaunch)
        acceptance.gate("a1_squat_fails_closed", acceptance.a1_squat)
        acceptance.gate("a1_ready_before_agent", acceptance.isolation)
        for gate, reason in unselected_path_failures(args.paths).items():
            checks.set(gate, False, reason)
        for path in args.paths:
            acceptance.gate(PATH_GATES[path], lambda p=path: acceptance.cli_path(p))
        acceptance.gate("a5_lifeline", acceptance.lifeline)
        acceptance.gate("a2p_d_no_broker_files", acceptance.finish)
        acceptance.gate("a11_apparmor", acceptance.a11)
    finally:
        acceptance.remove_plants()
    checks.fail_pending("not run: an earlier step failed before this gate could be proven")
    return acceptance.captured


def main(argv=None, stdin=None, stdout=None):
    argv = sys.argv[1:] if argv is None else list(argv)
    stdout = stdout if stdout is not None else sys.stdout
    if argv[:1] == ["--internal-holder"]:  # pragma: no cover - Linux child
        return holder_main(argv[1:])
    checks = Checks()
    try:
        args = parse_args(argv)
    except UsageError as error:
        checks.set("preconditions", False, "usage: %s" % error)
        checks.fail_pending("not run: usage error")
        text, _ok = render(checks, None)
        stdout.write(text + "\n")
        stdout.flush()
        return 2
    stream = stdin if stdin is not None else sys.stdin.buffer
    secret = read_secret(stream)
    checks.secret = secret
    captured = []
    if secret is None:
        checks.set("preconditions", False,
                   "no synthetic secret on stdin (one line, neev-syn-<printable ASCII>)")
        checks.fail_pending("not run: no synthetic secret")
    elif platform.system() != "Linux":
        checks.set("preconditions", False, "not Linux (%s): nothing was run" % platform.system())
        checks.fail_pending("not run: preconditions failed")
    elif os.geteuid() != 0:
        checks.set("preconditions", False, "euid %d, root required" % os.geteuid())
        checks.fail_pending("not run: preconditions failed")
    else:  # pragma: no cover - Linux, root
        captured = run_linux(args, secret, checks)
    text, ok = render(checks, secret, captured)
    stdout.write(text + "\n")
    stdout.flush()
    return 0 if ok else 1


if __name__ == "__main__":
    raise SystemExit(main())
