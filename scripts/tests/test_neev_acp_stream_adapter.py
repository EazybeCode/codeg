"""Tests for the generic ACP stream adapter.

Wire fixtures are copied VERBATIM from the official ACP v1 docs at tag
`schema-v1.23.0` (agentclientprotocol/agent-client-protocol,
docs/protocol/v1/{initialization,session-setup,prompt-turn,tool-calls}.mdx).
Only the JSON-RPC `id` of a response is rewritten to answer the id the adapter
actually sent. The one shape the docs give no example for (`agent_thought_chunk`)
is built from the schema: it is the same `ContentChunk` as
`agent_message_chunk`. No provider is called; the "agent" is a local script.
"""

import io
import json
import os
import signal
import stat
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path

OPERATIONS = Path(__file__).parents[1] / "operations"
sys.path.insert(0, str(OPERATIONS))

import neev_acp_stream_adapter as adapter  # noqa: E402

ADAPTER = str(OPERATIONS / "neev_acp_stream_adapter.py")

# --- official fixtures (verbatim) -------------------------------------------
INIT_RESPONSE = {"jsonrpc": "2.0", "id": 0, "result": {"protocolVersion": 1, "agentCapabilities": {"loadSession": True, "promptCapabilities": {"image": True, "audio": True, "embeddedContext": True}, "mcpCapabilities": {"http": True, "sse": True}}, "agentInfo": {"name": "my-agent", "title": "My Agent", "version": "1.0.0"}, "authMethods": []}}
SESSION_NEW_RESPONSE = {"jsonrpc": "2.0", "id": 1, "result": {"sessionId": "sess_abc123def456"}}
MESSAGE_CHUNK = {"jsonrpc": "2.0", "method": "session/update", "params": {"sessionId": "sess_abc123def456", "update": {"sessionUpdate": "agent_message_chunk", "messageId": "msg_agent_c42b9", "content": {"type": "text", "text": "I'll analyze your code for potential issues. Let me examine it..."}}}}
TOOL_CALL = {"jsonrpc": "2.0", "method": "session/update", "params": {"sessionId": "sess_abc123def456", "update": {"sessionUpdate": "tool_call", "toolCallId": "call_001", "title": "Analyzing Python code", "kind": "other", "status": "pending"}}}
USAGE_UPDATE = {"jsonrpc": "2.0", "method": "session/update", "params": {"sessionId": "sess_abc123def456", "update": {"sessionUpdate": "usage_update", "used": 53000, "size": 200000, "cost": {"amount": 0.045, "currency": "USD"}}}}
TOOL_IN_PROGRESS = {"jsonrpc": "2.0", "method": "session/update", "params": {"sessionId": "sess_abc123def456", "update": {"sessionUpdate": "tool_call_update", "toolCallId": "call_001", "status": "in_progress"}}}
TOOL_COMPLETED = {"jsonrpc": "2.0", "method": "session/update", "params": {"sessionId": "sess_abc123def456", "update": {"sessionUpdate": "tool_call_update", "toolCallId": "call_001", "status": "completed", "content": [{"type": "content", "content": {"type": "text", "text": "Analysis complete:\n- No syntax errors found\n- Consider adding type hints for better clarity\n- The function could benefit from error handling for empty lists"}}]}}}
PROMPT_RESPONSE = {"jsonrpc": "2.0", "id": 2, "result": {"stopReason": "end_turn"}}
CANCEL_NOTIFICATION = {"jsonrpc": "2.0", "method": "session/cancel", "params": {"sessionId": "sess_abc123def456"}}
PERMISSION_REQUEST = {"jsonrpc": "2.0", "id": 5, "method": "session/request_permission", "params": {"sessionId": "sess_abc123def456", "toolCall": {"toolCallId": "call_001"}, "options": [{"optionId": "allow-once", "name": "Allow once", "kind": "allow_once"}, {"optionId": "reject-once", "name": "Reject", "kind": "reject_once"}]}}
PERMISSION_ALLOW = {"jsonrpc": "2.0", "id": 5, "result": {"outcome": {"outcome": "selected", "optionId": "allow-once"}}}
PERMISSION_CANCELLED = {"jsonrpc": "2.0", "id": 5, "result": {"outcome": {"outcome": "cancelled"}}}
READ_TOOL_CALL = {"jsonrpc": "2.0", "method": "session/update", "params": {"sessionId": "sess_abc123def456", "update": {"sessionUpdate": "tool_call", "toolCallId": "call_001", "name": "read_file", "title": "Reading configuration file", "kind": "read", "status": "pending"}}}
DIFF_CONTENT = {"type": "diff", "path": "/home/user/project/src/config.json", "oldText": "{\n  \"debug\": false\n}", "newText": "{\n  \"debug\": true\n}"}
TERMINAL_CONTENT = {"type": "terminal", "terminalId": "term_xyz789"}
# Schema-derived (ContentChunk), no doc example exists:
THOUGHT_CHUNK = {"jsonrpc": "2.0", "method": "session/update", "params": {"sessionId": "sess_abc123def456", "update": {"sessionUpdate": "agent_thought_chunk", "content": {"type": "text", "text": "SECRET-REASONING"}}}}


class Capture:
    def __init__(self):
        self.lines = []

    def write(self, text):
        self.lines.extend(json.loads(t) for t in text.splitlines() if t)

    def flush(self):
        pass


def translate(*notifications):
    out = Capture()
    translator = adapter.Translator(adapter.Emitter(out))
    for note in notifications:
        translator.update(note["params"]["update"])
    return translator, out.lines


class TranslatorTests(unittest.TestCase):
    def test_message_chunk_becomes_codex_delta_and_completion(self):
        translator, lines = translate(MESSAGE_CHUNK)
        translator.finish(cancelled=False)
        text = MESSAGE_CHUNK["params"]["update"]["content"]["text"]
        self.assertEqual(lines, [
            {"type": "item.delta", "item": {"type": "agent_message", "id": "msg_agent_c42b9", "delta": text}},
            {"type": "item.completed", "item": {"type": "agent_message", "id": "msg_agent_c42b9", "text": text}},
        ])

    def test_tool_lifecycle_maps_to_command_execution(self):
        _, lines = translate(TOOL_CALL, TOOL_IN_PROGRESS, TOOL_COMPLETED)
        self.assertEqual(lines[0], {"type": "item.started", "item": {"type": "command_execution", "id": "call_001", "command": "Analyzing Python code"}})
        self.assertEqual(len(lines), 2)
        done = lines[1]["item"]
        self.assertEqual(done["exit_code"], 0)
        self.assertTrue(done["aggregated_output"].startswith("Analysis complete:"))

    def test_tool_breaks_message_so_next_text_is_new(self):
        _, lines = translate(MESSAGE_CHUNK, TOOL_CALL)
        self.assertEqual([l["type"] for l in lines], ["item.delta", "item.completed", "item.started"])

    def test_thought_chunks_never_written(self):
        translator, lines = translate(THOUGHT_CHUNK, USAGE_UPDATE)
        translator.finish(cancelled=False)
        self.assertEqual(lines, [])

    def test_open_tool_closed_as_failed_on_cancel(self):
        translator, lines = translate(READ_TOOL_CALL)
        translator.finish(cancelled=True)
        self.assertEqual(lines[-1]["item"]["exit_code"], 1)
        self.assertEqual(lines[-1]["item"]["aggregated_output"], "[cancelled]")

    def test_failed_status_and_non_text_content(self):
        update = {"sessionUpdate": "tool_call_update", "toolCallId": "call_001", "status": "failed", "content": [DIFF_CONTENT, TERMINAL_CONTENT]}
        _, lines = translate(READ_TOOL_CALL, {"params": {"update": update}})
        item = lines[-1]["item"]
        self.assertEqual(item["exit_code"], 1)
        self.assertEqual(item["aggregated_output"], "[diff] /home/user/project/src/config.json\n[terminal]")

    def test_open_tools_bounded(self):
        out = Capture()
        translator = adapter.Translator(adapter.Emitter(out))
        for i in range(adapter.MAX_OPEN_TOOLS + 10):
            translator.update({"sessionUpdate": "tool_call", "toolCallId": f"c{i}", "title": "t"})
        self.assertEqual(len(out.lines), adapter.MAX_OPEN_TOOLS)


class PolicyTests(unittest.TestCase):
    OPTIONS = PERMISSION_REQUEST["params"]["options"]

    def test_default_rejects(self):
        self.assertEqual(adapter.choose_permission(self.OPTIONS, "reject"), {"outcome": "selected", "optionId": "reject-once"})

    def test_grant_allows_once_matches_official_response(self):
        self.assertEqual(adapter.choose_permission(self.OPTIONS, "allow-once"), PERMISSION_ALLOW["result"]["outcome"])

    def test_never_allow_always_and_cancel_when_no_reject(self):
        options = [{"optionId": "always", "name": "Always", "kind": "allow_always"}]
        self.assertEqual(adapter.choose_permission(options, "allow-once"), PERMISSION_CANCELLED["result"]["outcome"])
        self.assertEqual(adapter.choose_permission("junk", "reject"), {"outcome": "cancelled"})

    def test_error_reasons_are_fixed_strings(self):
        self.assertEqual(adapter.error_reason({"code": -32000, "message": "token abc@x.com"}), "provider sign-in required")
        self.assertEqual(adapter.error_reason({"code": 7, "message": "/home/u/.gemini/oauth"}), "provider refused request")

    def test_env_allowlist_drops_keys_and_requires_home(self):
        env = adapter.agent_env({"HOME": "/vol/u1", "PATH": "/bin", "GEMINI_API_KEY": "k", "CURSOR_AUTH_TOKEN": "t", "DATABASE_URL": "x"})
        self.assertEqual(env, {"HOME": "/vol/u1", "PATH": "/bin"})
        with self.assertRaises(adapter.AdapterError):
            adapter.agent_env({"PATH": "/bin"})
        with self.assertRaises(adapter.AdapterError):
            adapter.agent_env({"HOME": "relative"})

    def test_capabilities_advertise_no_follow_up(self):
        caps = adapter.capabilities("cursor")
        self.assertFalse(caps["followUp"])
        self.assertFalse(caps["reasoning"])
        self.assertTrue(caps["cancel"])

    def test_outbound_messages_match_official_shapes(self):
        self.assertEqual(adapter.initialize_params()["protocolVersion"], 1)
        self.assertEqual(set(adapter.session_new_params("/w")), {"cwd", "mcpServers"})
        self.assertEqual(adapter.prompt_params("s", "hi"), {"sessionId": "s", "prompt": [{"type": "text", "text": "hi"}]})
        self.assertEqual(adapter.agent_argv("cursor", "/opt/agent"), ["/opt/agent", "acp"])
        self.assertEqual(adapter.agent_argv("gemini", "/opt/gemini"), ["/opt/gemini", "--acp"])
        self.assertEqual(adapter.agent_argv("opencode", "/opt/opencode"), ["/opt/opencode", "acp"])


FAKE_AGENT = r'''#!{python}
"""Scripted ACP agent replaying official fixtures. Scenario: ./scenario.json."""
import json, os, sys
scenario = json.load(open("scenario.json"))
json.dump(sorted(os.environ), open("env.json", "w"))
log = open("received.jsonl", "w")
def send(obj, rid=None):
    if rid is not None:
        obj = dict(obj, id=rid)
    sys.stdout.write(json.dumps(obj) + "\n"); sys.stdout.flush()
for chatter in scenario.get("chatter", []):
    sys.stdout.write(chatter + "\n"); sys.stdout.flush()
for line in sys.stdin:
    msg = json.loads(line); log.write(line); log.flush()
    method = msg.get("method")
    if method == "initialize":
        if scenario.get("init_hang"): continue
        send(scenario.get("init", INIT), msg["id"])
    elif method == "session/new":
        if scenario.get("session_chatter"):
            for chatter in scenario["session_chatter"]:
                sys.stdout.write(chatter + "\n"); sys.stdout.flush()
            continue
        if "session_result" in scenario:
            send({"jsonrpc": "2.0", "id": msg["id"], "result": scenario["session_result"]})
        elif "session_error" in scenario:
            send({"jsonrpc": "2.0", "id": msg["id"], "error": scenario["session_error"]})
        else:
            send(SESSION_NEW, msg["id"])
    elif method == "session/prompt":
        prompt_id = msg["id"]
        for note in scenario.get("updates", []):
            send(note)
        if scenario.get("permission"):
            send(PERMISSION)
            reply = json.loads(sys.stdin.readline()); log.write(json.dumps(reply) + "\n"); log.flush()
        if scenario.get("eof"): sys.exit(0)
        if scenario.get("hang"): continue
        send(PROMPT, msg["id"])
    elif method == "session/set_config_option":
        if "set_error" in scenario:
            send({"jsonrpc": "2.0", "id": msg["id"], "error": scenario["set_error"]})
        else:
            send({"jsonrpc": "2.0", "id": msg["id"], "result": scenario["set_result"]})
    elif method == "session/cancel":
        send({"jsonrpc": "2.0", "id": prompt_id, "result": {"stopReason": "cancelled"}})
'''


class EndToEndTests(unittest.TestCase):
    def run_adapter(self, scenario, provider="cursor", binary="agent", extra=(), env=None, signal_after=None, stdin=None, prompt="Can you analyze this code?"):
        tmp = tempfile.mkdtemp()
        path = os.path.join(tmp, binary)
        source = FAKE_AGENT.replace("{python}", sys.executable)
        source = "\n".join([source.split("\n", 1)[0],
                            "INIT = " + repr(INIT_RESPONSE),
                            "SESSION_NEW = " + repr(SESSION_NEW_RESPONSE),
                            "PERMISSION = " + repr(PERMISSION_REQUEST),
                            "PROMPT = " + repr(PROMPT_RESPONSE),
                            source.split("\n", 1)[1]])
        with open(path, "w") as handle:
            handle.write(source)
        os.chmod(path, stat.S_IRWXU)
        with open(os.path.join(tmp, "scenario.json"), "w") as handle:
            json.dump(scenario, handle)
        home = os.path.join(tmp, "home")
        os.mkdir(home)
        child_env = {"PATH": "/usr/bin:/bin", "HOME": home, "OPENAI_API_KEY": "leak", "GEMINI_API_KEY": "leak"}
        child_env.update(env or {})
        argv = [sys.executable, ADAPTER, "--provider", provider, "--agent", path,
                "--allowed-agent", path, "--cwd", tmp, *extra]
        if prompt is not None:
            argv += ["--", prompt]
        proc = subprocess.Popen(argv, stdin=subprocess.PIPE if stdin else subprocess.DEVNULL,
                                stdout=subprocess.PIPE, env=child_env, text=True)
        if stdin:
            proc.stdin.write(stdin)
            proc.stdin.flush()
        if signal_after is not None:
            time.sleep(signal_after)
            proc.send_signal(signal.SIGTERM)
        out, _ = proc.communicate(timeout=30)
        lines = [json.loads(l) for l in out.splitlines() if l.strip()]
        received_path = os.path.join(tmp, "received.jsonl")
        received = [json.loads(l) for l in open(received_path)] if os.path.exists(received_path) else []
        env_path = os.path.join(tmp, "env.json")
        seen_env = json.load(open(env_path)) if os.path.exists(env_path) else []
        return proc.returncode, lines, received, seen_env

    def test_full_turn(self):
        code, lines, received, seen_env = self.run_adapter(
            {"updates": [MESSAGE_CHUNK, THOUGHT_CHUNK, TOOL_CALL, TOOL_IN_PROGRESS, TOOL_COMPLETED, USAGE_UPDATE],
             "chatter": ["not json"]})
        self.assertEqual(code, 0)
        self.assertEqual([m.get("method") for m in received], ["initialize", "session/new", "session/prompt"])
        self.assertEqual(received[0]["params"]["clientCapabilities"]["terminal"], False)
        self.assertEqual(lines[-1], {"type": "turn.completed"})
        self.assertNotIn("SECRET-REASONING", json.dumps(lines))
        self.assertNotIn("OPENAI_API_KEY", seen_env)
        self.assertNotIn("GEMINI_API_KEY", seen_env)
        self.assertIn("HOME", seen_env)

    def test_permission_fails_closed_by_default(self):
        code, lines, received, _ = self.run_adapter({"permission": True})
        self.assertEqual(code, 0)
        self.assertEqual(received[-1]["result"]["outcome"], {"outcome": "selected", "optionId": "reject-once"})
        self.assertEqual(received[-1]["id"], 5)

    def test_permission_allow_once_with_grant(self):
        _, _, received, _ = self.run_adapter({"permission": True}, extra=("--permission-policy", "allow-once"))
        self.assertEqual(received[-1]["result"], PERMISSION_ALLOW["result"])

    def test_sigterm_sends_official_session_cancel(self):
        code, lines, received, _ = self.run_adapter({"hang": True, "updates": [TOOL_CALL]}, signal_after=1.5)
        self.assertEqual(code, 4)
        self.assertEqual(received[-1], CANCEL_NOTIFICATION)
        self.assertEqual(lines[-1], {"type": "turn.failed", "reason": "cancelled"})
        self.assertEqual(lines[-2]["item"]["exit_code"], 1)

    def test_auth_required_is_private_fixed_reason(self):
        code, lines, _, _ = self.run_adapter({"session_error": {"code": -32000, "message": "login as person@example.com at /home/x"}})
        self.assertEqual(code, 4)
        self.assertEqual(lines, [{"type": "turn.failed", "reason": "provider sign-in required"}])

    def test_eof_mid_turn(self):
        code, lines, _, _ = self.run_adapter({"eof": True, "updates": [MESSAGE_CHUNK]})
        self.assertEqual(code, 4)
        self.assertEqual(lines[-1], {"type": "turn.failed", "reason": "provider closed the stream"})
        self.assertEqual(lines[-2]["type"], "item.completed")

    def test_protocol_version_mismatch(self):
        init = json.loads(json.dumps(INIT_RESPONSE))
        init["result"]["protocolVersion"] = 2
        code, lines, _, _ = self.run_adapter({"init": init})
        self.assertEqual((code, lines[-1]["reason"]), (4, "unsupported provider protocol version"))

    def test_handshake_timeout(self):
        # In-process with a silent agent and a clock that jumps past the limit.
        read_fd, write_fd = os.pipe()

        class Child:
            stdin = io.BytesIO()
            stdout = os.fdopen(read_fd, "rb")

        clock = iter([0, 0, adapter.HANDSHAKE_TIMEOUT_SECS + 1] + [10 ** 6] * 50)
        out = Capture()
        args = adapter._parse(["--provider", "gemini", "--", "hi"])
        code = adapter.drive(Child, args, adapter.Emitter(out), "/w",
                             __import__("threading").Event(), now=lambda: next(clock))
        os.close(write_fd)
        self.assertEqual(code, 4)
        self.assertEqual(out.lines, [{"type": "turn.failed", "reason": "provider did not start the session"}])
        self.assertEqual(json.loads(Child.stdin.getvalue())["method"], "initialize")

    def test_steering_rejected_as_unsupported(self):
        code, lines, _, _ = self.run_adapter(
            {"updates": [MESSAGE_CHUNK]}, extra=("--enable-steering",),
            stdin=json.dumps({"requestId": "r1", "text": "also do X"}) + "\n")
        self.assertEqual(code, 0)
        steer = [l for l in lines if l["type"] == "neev.steer"]
        self.assertEqual(steer, [{"type": "neev.steer", "requestId": "r1", "accepted": False, "reason": adapter.FOLLOW_UP_UNSUPPORTED}])

    def test_binary_must_be_allowlisted_and_named(self):
        tmp = tempfile.mkdtemp()
        other = os.path.join(tmp, "agent")
        with open(other, "w") as handle:
            handle.write("#!/bin/sh\n")
        os.chmod(other, stat.S_IRWXU)
        with self.assertRaises(adapter.AdapterError):
            adapter.resolve_agent("cursor", other, [])
        with self.assertRaises(adapter.AdapterError):
            adapter.resolve_agent("cursor", "agent", ["agent"])
        with self.assertRaises(adapter.AdapterError):
            adapter.resolve_agent("gemini", other, [other])
        self.assertEqual(adapter.resolve_agent("cursor", other, [other]), other)



# OpenCode's session/new as observed on pinned 1.18.33 with NO sign-in (model list
# trimmed): anonymous "OpenCode Zen" models only.
OPENCODE_ANON_SESSION = {"sessionId": "sess_abc123def456", "configOptions": [
    {"id": "model", "name": "Model", "category": "model", "type": "select",
     "currentValue": "opencode/big-pickle",
     "options": [{"value": "opencode/big-pickle", "name": "OpenCode Zen/Big Pickle"}]},
    {"id": "mode", "name": "Session Mode", "category": "mode", "type": "select",
     "currentValue": "build", "options": [{"value": "build", "name": "build"}]}]}


def opencode_signed_in(current):
    session = json.loads(json.dumps(OPENCODE_ANON_SESSION))
    model = session["configOptions"][0]
    model["currentValue"] = current
    model["options"].append({"group": "openai", "name": "OpenAI", "options": [
        {"value": "openai/gpt-5.5", "name": "OpenAI/GPT-5.5"}]})
    return session


GEMINI_SIGNIN_CHATTER = [
    "\x1b[?1049h\x1b[2J\x1b[HPlease visit the following URL to authorize the application:",
    "",
    "https://accounts.google.com/o/oauth2/v2/auth?redirect_uri=x&state=SECRET",
]


class AcpProviderBehaviourTests(unittest.TestCase):
    """Provider-specific behaviour observed on the pinned binaries."""

    run_adapter = EndToEndTests.run_adapter

    def test_opencode_never_runs_on_an_anonymous_model(self):
        code, lines, received, _ = self.run_adapter(
            {"session_result": OPENCODE_ANON_SESSION}, provider="opencode", binary="opencode")
        self.assertEqual(code, 4)
        self.assertEqual(lines, [{"type": "turn.failed", "reason": "provider sign-in required"}])
        self.assertNotIn("session/prompt", [m.get("method") for m in received])

    def test_opencode_is_pinned_to_the_signed_in_upstream_before_prompting(self):
        session = opencode_signed_in("opencode/big-pickle")
        after = opencode_signed_in("openai/gpt-5.5")
        code, lines, received, _ = self.run_adapter(
            {"session_result": session, "set_result": {"configOptions": after["configOptions"]}},
            provider="opencode", binary="opencode")
        self.assertEqual(code, 0)
        methods = [m.get("method") for m in received]
        self.assertEqual(methods, ["initialize", "session/new", "session/set_config_option",
                                   "session/prompt"])
        self.assertEqual(received[2]["params"], {"sessionId": "sess_abc123def456",
                                                 "configId": "model", "value": "openai/gpt-5.5"})
        self.assertEqual(lines[-1], {"type": "turn.completed"})

    def test_opencode_already_on_the_upstream_needs_no_change(self):
        code, _, received, _ = self.run_adapter(
            {"session_result": opencode_signed_in("openai/gpt-5.5")},
            provider="opencode", binary="opencode")
        self.assertEqual(code, 0)
        self.assertNotIn("session/set_config_option", [m.get("method") for m in received])

    def test_opencode_refused_model_change_fails_closed(self):
        code, lines, received, _ = self.run_adapter(
            {"session_result": opencode_signed_in("opencode/big-pickle"),
             "set_error": {"code": -32602, "message": "nope"}},
            provider="opencode", binary="opencode")
        self.assertEqual(code, 4)
        self.assertEqual(lines[-1]["reason"], "provider sign-in required")
        self.assertNotIn("session/prompt", [m.get("method") for m in received])

    def test_gemini_signin_prompt_fails_fast_and_never_relays_the_url(self):
        started = time.monotonic()
        code, lines, _, _ = self.run_adapter(
            {"session_chatter": GEMINI_SIGNIN_CHATTER}, provider="gemini", binary="gemini")
        self.assertLess(time.monotonic() - started, 20)
        self.assertEqual(code, 4)
        self.assertEqual(lines, [{"type": "turn.failed", "reason": "provider sign-in required"}])
        self.assertNotIn("accounts.google.com", json.dumps(lines))

    def test_fixed_headless_env_reaches_the_agent_and_keys_do_not(self):
        _, _, _, seen_env = self.run_adapter({}, provider="gemini", binary="gemini",
                                             env={"NO_BROWSER": "false", "GOOGLE_API_KEY": "k"})
        self.assertIn("NO_BROWSER", seen_env)
        self.assertIn("GEMINI_FORCE_FILE_STORAGE", seen_env)
        self.assertNotIn("GOOGLE_API_KEY", seen_env)
        env = adapter.agent_env({"HOME": "/h", "NO_BROWSER": "false"}, "gemini")
        self.assertEqual(env["NO_BROWSER"], "true")
        self.assertEqual(adapter.agent_env({"HOME": "/h"}, "cursor")["NO_OPEN_BROWSER"], "1")

    def test_context_is_a_leading_delimited_block(self):
        code, _, received, _ = self.run_adapter({}, extra=("--context", "Be brief."))
        self.assertEqual(code, 0)
        blocks = received[2]["params"]["prompt"]
        self.assertEqual(blocks[0], {"type": "text",
                                     "text": "<neev-instructions>\nBe brief.\n</neev-instructions>"})
        self.assertEqual(blocks[1], {"type": "text", "text": "Can you analyze this code?"})

    def test_probe_supported_disconnected_and_unavailable(self):
        code, lines, received, _ = self.run_adapter({}, extra=("--probe",), prompt=None)
        self.assertEqual((code, lines), (0, [{"type": "neev.account", "status": "supported"}]))
        self.assertNotIn("session/prompt", [m.get("method") for m in received])
        code, lines, _, _ = self.run_adapter(
            {"session_error": {"code": -32000, "message": "x"}}, extra=("--probe",), prompt=None)
        self.assertEqual((code, lines), (0, [{"type": "neev.account", "status": "disconnected"}]))
        code, lines, _, _ = self.run_adapter(
            {"session_chatter": GEMINI_SIGNIN_CHATTER}, provider="gemini", binary="gemini",
            extra=("--probe",), prompt=None)
        self.assertEqual((code, lines), (0, [{"type": "neev.account", "status": "disconnected"}]))
        # OpenCode anonymous session is NOT a supported account (no fake success).
        code, lines, _, _ = self.run_adapter(
            {"session_result": OPENCODE_ANON_SESSION}, provider="opencode", binary="opencode",
            extra=("--probe",), prompt=None)
        self.assertEqual((code, lines), (0, [{"type": "neev.account", "status": "disconnected"}]))
        code, lines, _, _ = self.run_adapter(
            {"session_error": {"code": -32603, "message": "x"}}, extra=("--probe",), prompt=None)
        self.assertEqual((code, lines), (4, [{"type": "neev.account", "status": "unavailable"}]))

    def test_probe_refuses_a_prompt(self):
        code, lines, _, _ = self.run_adapter({}, extra=("--probe",))
        self.assertEqual((code, lines), (2, [{"type": "neev.account", "status": "unavailable"}]))

    def test_model_choice_is_pure_and_prefix_bound(self):
        self.assertEqual(adapter.model_choice(None, "openai/"), ("missing", None))
        self.assertEqual(adapter.model_choice(OPENCODE_ANON_SESSION["configOptions"], "openai/"),
                         ("missing", None))
        self.assertEqual(
            adapter.model_choice(opencode_signed_in("x")["configOptions"], "openai/"),
            ("set", ("model", "openai/gpt-5.5")))
        # A value merely CONTAINING the prefix is not the upstream.
        tricky = [{"id": "model", "category": "model", "currentValue": "zen/openai/x",
                   "options": [{"value": "zen/openai/x"}]}]
        self.assertEqual(adapter.model_choice(tricky, "openai/"), ("missing", None))


if __name__ == "__main__":
    unittest.main()


STUBBORN_AGENT = r'''#!{python}
# Cursor-shaped: answers the handshake, refuses the session (-32000), then does
# NOT exit on stdin EOF, and leaves a grandchild holding stdout open.
import json, subprocess, sys, time
subprocess.Popen(["sleep", "60"], stdout=sys.stdout, stderr=subprocess.DEVNULL)  # holds the pipe
for line in sys.stdin:
    msg = json.loads(line)
    if msg.get("method") == "initialize":
        out = {{"jsonrpc": "2.0", "id": msg["id"], "result": {{"protocolVersion": 1,
               "agentCapabilities": {{}}, "authMethods": [{{"id": "cursor_login"}}]}}}}
    elif msg.get("method") == "session/new":
        out = {{"jsonrpc": "2.0", "id": msg["id"], "error": {{"code": -32000,
               "message": "Authentication required"}}}}
    else:
        continue
    sys.stdout.write(json.dumps(out) + "\n"); sys.stdout.flush()
time.sleep(60)
'''


class ShutdownTests(unittest.TestCase):
    def test_a_probe_exits_even_when_the_agent_and_a_grandchild_keep_stdout(self):
        tmp = tempfile.mkdtemp()
        agent = os.path.join(tmp, "agent")
        with open(agent, "w") as handle:
            handle.write(STUBBORN_AGENT.format(python=sys.executable))
        os.chmod(agent, stat.S_IRWXU)
        home = os.path.join(tmp, "home")
        os.mkdir(home)
        started = time.monotonic()
        out_path = os.path.join(tmp, "out")
        with open(out_path, "w") as out, open(os.devnull, "w") as err:
            proc = subprocess.Popen(
                [sys.executable, ADAPTER, "--provider", "cursor", "--agent", agent,
                 "--allowed-agent", agent, "--cwd", tmp, "--probe"],
                stdout=out, stderr=err, env={"PATH": "/usr/bin:/bin", "HOME": home})
            try:
                code = proc.wait(timeout=40)
            except subprocess.TimeoutExpired:
                proc.kill()
                self.fail("adapter never exited")
        elapsed = time.monotonic() - started
        with open(out_path) as handle:
            self.assertEqual(handle.read().strip(),
                             '{"type": "neev.account", "status": "disconnected"}')
        self.assertEqual(code, 0)
        # Observed on the pinned image before the fix: verdict printed, then a
        # hang until killed from outside.
        self.assertLess(elapsed, adapter.CANCEL_GRACE_SECS + 8)
