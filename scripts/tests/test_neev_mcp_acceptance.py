"""Pure parts of the fail-closed Linux acceptance for shared integrations, and
the loopback fake model it drives the CLIs with.

Nothing here needs Linux or root: the privileged gates only run inside the
acceptance container. These tests pin the parts that decide PASS/FAIL — the
argument and result contract, the secret discipline, and the structured
tool-event parsers — plus the fake model over real local HTTP."""

import http.client
import io
import json
import os
import platform
import sys
import unittest

sys.dont_write_bytecode = True
sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "operations"))

import neev_mcp_acceptance as acc  # noqa: E402
import neev_mcp_fake_model as fake  # noqa: E402

SECRET = "neev-syn-" + "0123456789abcdef" * 3
MARKER = "neev-accept-marker-00112233aabbccdd"
TOOL = "mcp__accept__echo"


def jl(*events):
    return [json.dumps(event).encode() for event in events]


# --------------------------------------------------------------------------- #
# Arguments, result document, secret
# --------------------------------------------------------------------------- #


class ArgumentTests(unittest.TestCase):
    def test_defaults_select_all_four_paths(self):
        args = acc.parse_args(["--fixture", "/opt/neev-accept", "--secret-stdin"])
        self.assertEqual(args.paths, acc.PATHS)
        self.assertEqual(args.slot, "a")
        self.assertEqual(args.provider_ipv4, [acc.DEFAULT_PROVIDER_IPV4])

    def test_subset_is_canonical_order_and_unknown_refuses(self):
        self.assertEqual(acc.select_paths("codex-exec, claude-raw"), ("claude-raw", "codex-exec"))
        for bad in ("", " , ", "claude", "claude-raw,acp"):
            with self.assertRaises(acc.UsageError):
                acc.select_paths(bad)

    def test_secret_only_from_stdin(self):
        with self.assertRaises(acc.UsageError):
            acc.parse_args(["--fixture", "/opt/neev-accept"])
        with self.assertRaises(acc.UsageError):
            acc.parse_args(["--secret-stdin"])
        with self.assertRaises(acc.UsageError):
            acc.parse_args(["--fixture", "/x", "--secret-stdin", "--secret", SECRET])

    def test_unselected_paths_fail_never_skip(self):
        failures = acc.unselected_path_failures(("claude-raw",))
        self.assertEqual(set(failures), {"a10_claude_adapter", "a10_codex_exec",
                                         "a10_codex_app_server"})
        self.assertTrue(all("mandatory" in reason for reason in failures.values()))
        self.assertEqual(acc.unselected_path_failures(acc.PATHS), {})

    def test_read_secret_accepts_one_synthetic_line_only(self):
        self.assertEqual(acc.read_secret(io.BytesIO((SECRET + "\nmore\n").encode())), SECRET)
        self.assertEqual(acc.read_secret(io.StringIO(SECRET + "\r\n")), SECRET)
        for bad in (b"", b"\n", b"ghp_realtoken0123456789abcdef\n", b"neev-syn-short\n",
                    b"neev-syn-" + b"\xff" * 20 + b"\n", ("neev-syn-" + "a b" * 10 + "\n").encode()):
            self.assertIsNone(acc.read_secret(io.BytesIO(bad)), bad)


class ResultTests(unittest.TestCase):
    def test_every_gate_starts_failed(self):
        checks = acc.Checks()
        self.assertFalse(checks.ok())
        self.assertEqual(set(checks.as_dict()), set(acc.GATES))
        self.assertTrue(all(item == {"ok": False, "detail": "not run"}
                            for item in checks.as_dict().values()))

    def test_ok_only_when_every_gate_passed(self):
        checks = acc.Checks(secret=SECRET)
        for name in acc.GATES[:-1]:
            checks.set(name, True, "fine")
        text, ok = acc.render(checks, SECRET, captured=[b"clean output"])
        doc = json.loads(text)
        self.assertTrue(ok)
        self.assertEqual(set(doc), {"ok", "secret_sha16", "checks"})
        self.assertIs(doc["ok"], True)
        self.assertEqual(doc["secret_sha16"], acc.sha16(SECRET))
        self.assertEqual(len(doc["secret_sha16"]), 16)
        checks.set("a11_apparmor", False, "not observable")
        self.assertFalse(json.loads(acc.render(checks, SECRET)[0])["ok"])

    def test_truthy_non_true_is_not_a_pass(self):
        checks = acc.Checks(names=("x",))
        checks.set("x", "yes", "")
        self.assertFalse(checks.ok())

    def test_secret_in_captured_output_fails_and_is_never_printed(self):
        checks = acc.Checks(secret=SECRET)
        for name in acc.GATES:
            checks.set(name, True, "token was " + SECRET)
        text, ok = acc.render(checks, SECRET, captured=[b"x", ("leak " + SECRET).encode()])
        self.assertFalse(ok)
        self.assertNotIn(SECRET, text)
        doc = json.loads(text)
        self.assertFalse(doc["checks"]["no_secret_in_output"]["ok"])
        self.assertIn("[secret]", doc["checks"]["preconditions"]["detail"])

    def test_no_secret_read_is_a_failure(self):
        text, ok = acc.render(acc.Checks(), None)
        self.assertFalse(ok)
        self.assertIsNone(json.loads(text)["secret_sha16"])


@unittest.skipIf(platform.system() == "Linux", "main() would run the real gates on Linux")
class MainOffLinuxTests(unittest.TestCase):
    def run_main(self, argv, stdin):
        out = io.StringIO()
        rc = acc.main(argv, stdin=io.BytesIO(stdin), stdout=out)
        lines = out.getvalue().splitlines()
        self.assertEqual(len(lines), 1, "exactly one JSON object")
        return rc, json.loads(lines[0]), out.getvalue()

    def test_off_linux_every_gate_fails_and_only_the_sha_is_reported(self):
        rc, doc, raw = self.run_main(["--fixture", "/opt/neev-accept", "--secret-stdin"],
                                     (SECRET + "\n").encode())
        self.assertEqual(rc, 1)
        self.assertIs(doc["ok"], False)
        self.assertEqual(doc["secret_sha16"], acc.sha16(SECRET))
        self.assertEqual(set(doc["checks"]), set(acc.GATES))
        self.assertTrue(all(item["ok"] is False for name, item in doc["checks"].items()
                            if name != "no_secret_in_output"))
        self.assertIn("not Linux", doc["checks"]["preconditions"]["detail"])
        self.assertNotIn(SECRET, raw)

    def test_usage_error_is_json_and_exit_2(self):
        rc, doc, _raw = self.run_main(["--fixture", "/opt/neev-accept"], b"")
        self.assertEqual(rc, 2)
        self.assertIs(doc["ok"], False)
        self.assertIn("usage", doc["checks"]["preconditions"]["detail"])

    def test_missing_or_real_looking_secret_fails(self):
        for stdin in (b"", b"sk-ant-realkey-0123456789abcdef\n"):
            rc, doc, raw = self.run_main(["--fixture", "/opt/neev-accept", "--secret-stdin"], stdin)
            self.assertEqual(rc, 1)
            self.assertIsNone(doc["secret_sha16"])
            self.assertNotIn("sk-ant", raw)


# --------------------------------------------------------------------------- #
# Tool-event parsers
# --------------------------------------------------------------------------- #


def claude_init():
    return {"type": "system", "subtype": "init", "tools": ["Bash", TOOL],
            "mcp_servers": [{"name": "accept", "status": "connected"}]}


def claude_use(marker=MARKER, use_id="toolu_1"):
    return {"type": "assistant", "message": {"role": "assistant", "content": [
        {"type": "tool_use", "id": use_id, "name": TOOL, "input": {"text": marker}}]}}


def claude_result(text, use_id="toolu_1", is_error=False, as_list=True):
    content = [{"type": "text", "text": text}] if as_list else text
    return {"type": "user", "message": {"role": "user", "content": [
        {"type": "tool_result", "tool_use_id": use_id, "content": content, "is_error": is_error}]}}


def claude_final(denials=(), is_error=False):
    return {"type": "result", "subtype": "error_during_execution" if is_error else "success",
            "is_error": is_error, "permission_denials": list(denials)}


class ClaudeParserTests(unittest.TestCase):
    def parse(self, *events):
        return acc.parse_claude_stream(jl(*events), TOOL, MARKER)

    def test_tool_use_with_its_echo_result_passes(self):
        ok, detail = self.parse(claude_init(), claude_use(), claude_result("echo:" + MARKER),
                                claude_final())
        self.assertTrue(ok, detail)
        self.assertIn("status=connected", detail)
        ok, _ = self.parse(claude_use(), claude_result("echo:" + MARKER, as_list=False),
                           claude_final())
        self.assertTrue(ok)

    def test_denied_permission_fails(self):
        denial = ("Claude requested permissions to use mcp__accept__echo, "
                  "but you haven't granted it yet.")
        ok, detail = self.parse(claude_init(), claude_use(),
                                claude_result(denial, is_error=True),
                                claude_final(denials=[{"tool_name": TOOL, "tool_use_id": "toolu_1",
                                                       "tool_input": {"text": MARKER}}]))
        self.assertFalse(ok)
        self.assertIn("denial", detail)

    def test_names_only_in_system_init_and_prompt_fail(self):
        prompt = {"type": "user", "message": {"role": "user", "content": [
            {"type": "text", "text": "call %s with echo:%s" % (TOOL, MARKER)}]}}
        text = {"type": "assistant", "message": {"role": "assistant", "content": [
            {"type": "text", "text": "I would call %s; echo:%s" % (TOOL, MARKER)}]}}
        ok, detail = self.parse(claude_init(), prompt, text, claude_final())
        self.assertFalse(ok)
        self.assertIn("no parsed tool_use", detail)

    def test_missing_wrong_or_errored_result_fails(self):
        self.assertFalse(self.parse(claude_use(), claude_final())[0])
        self.assertFalse(self.parse(claude_use(), claude_result("echo:other"), claude_final())[0])
        self.assertFalse(self.parse(claude_use(marker="neev-accept-marker-x"),
                                    claude_result("echo:" + MARKER), claude_final())[0])
        self.assertFalse(self.parse(claude_use(), claude_result("echo:" + MARKER, is_error=True),
                                    claude_final())[0])
        self.assertFalse(self.parse(claude_use(), claude_result("echo:" + MARKER),
                                    claude_final(is_error=True))[0])
        self.assertFalse(self.parse(claude_use(), claude_result("echo:" + MARKER))[0])

    def test_other_tool_or_garbage_lines_do_not_count(self):
        other = {"type": "assistant", "message": {"content": [
            {"type": "tool_use", "id": "toolu_1", "name": "mcp__other__echo",
             "input": {"text": MARKER}}]}}
        lines = [b"not json", b"[1,2]"] + jl(other, claude_result("echo:" + MARKER),
                                             claude_final())
        self.assertFalse(acc.parse_claude_stream(lines, TOOL, MARKER)[0])


def codex_item(phase="item.completed", status="completed", text=None, error=None,
               arguments=None, kind="mcp_tool_call", server="accept", tool="echo"):
    item = {"id": "item_1", "type": kind, "server": server, "tool": tool,
            "arguments": {"text": MARKER} if arguments is None else arguments,
            "status": status, "error": error,
            "result": None if text is None else {"content": [{"type": "text", "text": text}],
                                                 "structured_content": None}}
    return {"type": phase, "item": item}


class CodexParserTests(unittest.TestCase):
    def parse(self, *events):
        return acc.parse_codex_stream(jl(*events), "accept", "echo", MARKER)

    def test_exec_mcp_tool_call_passes(self):
        ok, detail = self.parse({"type": "thread.started"},
                                codex_item("item.started", "in_progress"),
                                codex_item(text="echo:" + MARKER), {"type": "turn.completed"})
        self.assertTrue(ok, detail)
        ok, _ = self.parse(codex_item(text="echo:" + MARKER,
                                      arguments=json.dumps({"text": MARKER})),
                           {"type": "turn.completed"})
        self.assertTrue(ok)

    def test_cancelled_or_failed_call_fails(self):
        self.assertFalse(self.parse(codex_item(status="failed",
                                               error={"message": "user cancelled MCP tool call"}),
                                    {"type": "turn.completed"})[0])
        self.assertFalse(self.parse(codex_item(text="user cancelled MCP tool call"),
                                    {"type": "turn.completed"})[0])
        self.assertFalse(self.parse(codex_item(text="echo:" + MARKER), {"type": "turn.failed"})[0])
        self.assertFalse(self.parse(codex_item(text="echo:" + MARKER))[0])

    def test_names_only_in_messages_or_commands_fail(self):
        ok, detail = self.parse(
            {"type": "item.completed", "item": {"type": "agent_message", "id": "m",
                                                "text": "%s -> echo:%s" % (TOOL, MARKER)}},
            {"type": "item.completed", "item": {"type": "command_execution", "id": "c",
                                                "command": "echo " + TOOL,
                                                "aggregated_output": "echo:" + MARKER}},
            {"type": "turn.completed"})
        self.assertFalse(ok)
        self.assertIn("no parsed MCP tool-call item", detail)

    def test_wrong_server_tool_or_marker_fail(self):
        self.assertFalse(self.parse(codex_item(server="other", text="echo:" + MARKER),
                                    {"type": "turn.completed"})[0])
        self.assertFalse(self.parse(codex_item(tool="secret_present", text="echo:" + MARKER),
                                    {"type": "turn.completed"})[0])
        self.assertFalse(self.parse(codex_item(text="echo:nope"), {"type": "turn.completed"})[0])

    def test_app_server_adapter_and_raw_notifications(self):
        camel = codex_item(text="echo:" + MARKER, kind="mcpToolCall")
        self.assertTrue(self.parse(camel, {"type": "turn.completed"})[0])
        raw = {"method": "item/completed", "params": {"item": dict(camel["item"])}}
        done = {"method": "turn/completed", "params": {"turn": {"status": "completed"}}}
        self.assertTrue(self.parse(raw, done)[0])
        interrupted = {"method": "turn/completed", "params": {"turn": {"status": "interrupted"}}}
        self.assertFalse(self.parse(raw, interrupted)[0])

    def test_adapter_stream_without_mcp_items_is_unprovable(self):
        # Today's adapter forwards only agentMessage/commandExecution items.
        ok, detail = self.parse({"type": "item.delta", "item": {"type": "agent_message",
                                                                "id": "m", "delta": "x"}},
                                {"type": "item.completed", "item": {"type": "agent_message",
                                                                    "id": "m", "text": "done"}},
                                {"type": "turn.completed"})
        self.assertFalse(ok)
        self.assertIn("carries none", detail)


class PathEvaluationTests(unittest.TestCase):
    def summary(self, **over):
        base = {"type": "neev.fake_model", "requests": 2, "toolCallIssued": 1,
                "toolResultSeen": True, "toolResultHasMarker": True, "toolResultIsError": False,
                "timedOut": False, "offeredTools": [TOOL]}
        base.update(over)
        return base

    def test_needs_both_stream_and_model_evidence(self):
        good = [claude_init(), claude_use(), claude_result("echo:" + MARKER), claude_final()]
        self.assertTrue(acc.evaluate_path("claude-raw", jl(*good, self.summary()), MARKER)[0])
        self.assertFalse(acc.evaluate_path("claude-raw", jl(*good), MARKER)[0])
        self.assertFalse(acc.evaluate_path(
            "claude-adapter", jl(*good, self.summary(toolResultHasMarker=False)), MARKER)[0])
        self.assertFalse(acc.evaluate_path(
            "claude-adapter", jl(*good, self.summary(timedOut=True)), MARKER)[0])
        codex = [codex_item(text="echo:" + MARKER), {"type": "turn.completed"}, self.summary()]
        self.assertTrue(acc.evaluate_path("codex-exec", jl(*codex), MARKER)[0])
        self.assertFalse(acc.evaluate_path("codex-exec", jl(*good, self.summary()), MARKER)[0])


class KernelLogTests(unittest.TestCase):
    def test_kmsg_and_denial_count(self):
        records = [acc.kmsg_parse(b'6,10,1,-;apparmor="DENIED" operation="open" '
                                  b'profile="neev-agent-accept-1234abcd" name="/x"\n'),
                   acc.kmsg_parse('6,11,2,-;apparmor="DENIED" profile="neev-agent-accept-1234abcd"'),
                   acc.kmsg_parse('6,12,3,-;apparmor="DENIED" profile="other"'),
                   acc.kmsg_parse('6,13,4,-;apparmor="ALLOWED" profile="neev-agent-accept-1234abcd"')]
        self.assertEqual(records[0][0], 10)
        self.assertIsNone(acc.kmsg_parse(b"garbage"))
        self.assertEqual(acc.count_denials(records, 9, "neev-agent-accept-1234abcd"), 2)
        self.assertEqual(acc.count_denials(records, 10, "neev-agent-accept-1234abcd"), 1)

    def test_profile_label(self):
        self.assertEqual(acc.profile_of("neev-agent-accept-1 (enforce)\n"),
                         ("neev-agent-accept-1", "enforce"))
        self.assertEqual(acc.profile_of("unconfined"), ("unconfined", None))
        self.assertEqual(acc.profile_of(None), (None, None))


class RuntimeShapeTests(unittest.TestCase):
    """The acceptance drives the REAL launcher builders with exactly what Rust emits."""

    def test_bridge_config_matches_rust(self):
        self.assertEqual(acc.claude_mcp_config(), {"mcpServers": {"accept": {
            "command": "/usr/bin/python3",
            "args": ["-I", "/opt/neev/scripts/operations/neev_mcp_bridge.py", "accept"]}}})
        self.assertEqual(acc.codex_mcp_overrides(), [
            'mcp_servers.accept.command="/usr/bin/python3"',
            'mcp_servers.accept.args=["-I","/opt/neev/scripts/operations/neev_mcp_bridge.py",'
            '"accept"]'])

    def test_fake_model_location_is_root_owned_and_unwritable(self):
        import collections
        import stat as st

        Info = collections.namedtuple("Info", "st_mode st_uid")
        table = {"/ops/m.py": Info(st.S_IFREG | 0o664, 0), "/fx/m.py": Info(st.S_IFREG | 0o444, 0),
                 "/bad/m.py": Info(st.S_IFREG | 0o444, 1000), "/ln/m.py": Info(st.S_IFLNK | 0o777, 0)}

        def lstat(path):
            if path not in table:
                raise FileNotFoundError(path)
            return table[path]

        pick = lambda *paths: acc.fake_model_path("/fx", candidates=paths, lstat=lstat)  # noqa: E731
        self.assertEqual(pick("/ops/m.py", "/fx/m.py"), "/fx/m.py")  # group-writable refused
        self.assertIsNone(pick("/bad/m.py", "/ln/m.py", "/none/m.py"))

    def test_bundle_is_valid_for_the_broker_and_carries_no_newline(self):
        import neev_mcp_broker as broker

        raw = acc.bundle_for(SECRET)
        self.assertNotIn(b"\n", raw)
        spec = broker.PackageSpec(argv=("/usr/bin/python3",), env_names=("NEEV_ACCEPT_TOKEN",))
        servers = broker.parse_bundle(raw, packages={broker.ACCEPT_FIXTURE_PACKAGE: spec})
        self.assertEqual(servers[0]["key"], "accept")
        self.assertEqual(broker.ACCEPT_FIXTURE_PACKAGE, "accept-stub")

    def test_launcher_builders_accept_the_paths(self):
        import neev_agent_launch as launch

        claude = ["/usr/local/bin/claude", "-p", acc.PROMPT, "--output-format", "stream-json",
                  "--verbose", "--mcp-config", "/data/agents/run-x/neev-library/mcp.json",
                  "--allowedTools", "mcp__accept"]
        adapter = launch.claude_stream_argv("claude", claude, enable_steering=True)
        self.assertEqual(adapter[2], launch.CLAUDE_ADAPTER)
        self.assertIn("--allowedTools", adapter)
        self.assertEqual(launch.externally_sandboxed_argv("claude", claude), claude)
        codex = ["/usr/local/bin/codex", "exec", "--json", acc.PROMPT]
        for override in acc.codex_mcp_overrides():
            codex += ["-c", override]
        codex += ["--skip-git-repo-check"]
        self.assertEqual(launch.app_server_argv("codex", codex, enable_steering=True)[2],
                         launch.ADAPTER)
        exec_argv = launch.externally_sandboxed_argv("codex", codex)
        self.assertEqual(exec_argv[-2:], ["--", acc.PROMPT])
        config = acc.codex_fake_config(47811)
        self.assertIn('base_url = "http://127.0.0.1:47811/v1"', config)
        self.assertIn('wire_api = "responses"', config)


# --------------------------------------------------------------------------- #
# Fake model over local HTTP
# --------------------------------------------------------------------------- #


def sse_events(body):
    events = []
    for frame in body.decode("utf-8").split("\n\n"):
        for line in frame.splitlines():
            if line.startswith("data: ") and line[6:] != "[DONE]":
                events.append(json.loads(line[6:]))
    return events


class BrokerProcessProofTests(unittest.TestCase):
    """The `broker_process` gate on current kernels: `/proc/<pid>` keeps the
    task's uid when it is not dumpable (measured on 7.0.0); `/proc/<pid>/status`
    is re-owned to root, and a same-uid reader is refused `environ`."""

    def proof(self, status_owner, denied, **overrides):
        class Stat:
            def __init__(self, uid):
                self.st_uid = uid

        def stat(path):
            self.assertTrue(path.endswith("/status"), "must not read the /proc/<pid> dir owner")
            if status_owner is None:
                raise FileNotFoundError(path)
            return Stat(status_owner)

        original = acc.proc_status
        acc.proc_status = lambda pid: dict({
            "Uid": "63000\t63000\t63000\t63000", "Gid": "63000\t63000\t63000\t63000",
            "CapEff": "0", "CapPrm": "0", "CapBnd": "0", "CapAmb": "0",
            "NoNewPrivs": "1", "Umask": "0077"}, **overrides)
        try:
            return acc.process_proof(42, 63000, stat=stat,
                                            environ_probe=lambda pid, uid: denied)
        finally:
            acc.proc_status = original

    def test_kernel_seven_shape_passes(self):
        broker = self.proof(status_owner=0, denied=True)
        self.assertEqual(broker["status_owner"], 0)
        self.assertTrue(acc.broker_process_ok(broker, broker, cmdline_secret=False))

    def test_every_unproven_field_fails_closed(self):
        good = self.proof(status_owner=0, denied=True)
        for broker in (self.proof(status_owner=63000, denied=True),   # still dumpable
                       self.proof(status_owner=0, denied=False),      # environ readable
                       self.proof(status_owner=0, denied=None),       # not measurable
                       self.proof(status_owner=None, denied=True),    # gone
                       self.proof(status_owner=0, denied=True, Umask="0022"),
                       self.proof(status_owner=0, denied=True, CapEff="1"),
                       None):
            with self.subTest(broker=broker):
                self.assertFalse(acc.broker_process_ok(broker, good, cmdline_secret=False))
        self.assertFalse(acc.broker_process_ok(good, good, cmdline_secret=True))
        self.assertFalse(acc.broker_process_ok(good, None, cmdline_secret=False))

    def test_the_environ_probe_is_never_a_pass_off_linux_root(self):
        if os.geteuid() == 0:
            self.skipTest("root: the real probe would run")
        # As a non-root user setuid fails in the child: the probe reports None or
        # False, never True.
        self.assertIsNot(acc.same_uid_environ_denied(os.getpid(), 63000), True)


class FakeModelHTTPTests(unittest.TestCase):
    def setUp(self):
        self.server, self.state = fake.make_server(0, MARKER, TOOL)
        self.port = self.server.server_address[1]

    def tearDown(self):
        self.server.shutdown()
        self.server.server_close()

    def post(self, path, body):
        conn = http.client.HTTPConnection("127.0.0.1", self.port, timeout=10)
        conn.request("POST", path, body=json.dumps(body),
                     headers={"Content-Type": "application/json"})
        response = conn.getresponse()
        data = response.read()
        conn.close()
        return response.status, response.getheader("Content-Type"), data

    def test_anthropic_stream_tool_use_then_text(self):
        tools = [{"name": "Bash", "input_schema": {}}, {"name": TOOL, "input_schema": {}}]
        first = {"model": "claude-x", "stream": True, "tools": tools,
                 "messages": [{"role": "user", "content": "go"}]}
        status, ctype, body = self.post("/v1/messages?beta=true", first)
        self.assertEqual(status, 200)
        self.assertTrue(ctype.startswith("text/event-stream"))
        events = sse_events(body)
        start = [e for e in events if e["type"] == "content_block_start"][0]["content_block"]
        self.assertEqual((start["type"], start["name"]), ("tool_use", TOOL))
        delta = [e for e in events if e["type"] == "content_block_delta"][0]["delta"]
        self.assertEqual(json.loads(delta["partial_json"]), {"text": MARKER})
        stop = [e for e in events if e["type"] == "message_delta"][0]["delta"]["stop_reason"]
        self.assertEqual(stop, "tool_use")
        second = dict(first, messages=first["messages"] + [
            {"role": "assistant", "content": [{"type": "tool_use", "id": start["id"],
                                               "name": TOOL, "input": {"text": MARKER}}]},
            {"role": "user", "content": [{"type": "tool_result", "tool_use_id": start["id"],
                                          "content": [{"type": "text",
                                                       "text": "echo:" + MARKER}]}]}])
        _status, _ctype, body = self.post("/v1/messages", second)
        events = sse_events(body)
        self.assertEqual([e for e in events if e["type"] == "content_block_start"][0]
                         ["content_block"]["type"], "text")
        summary = self.state.summary()
        self.assertEqual((summary["toolCallIssued"], summary["toolResultSeen"],
                          summary["toolResultHasMarker"]), (1, True, True))
        self.assertTrue(acc.fake_model_ok(dict(summary, timedOut=False))[0])

    def test_anthropic_denied_result_is_not_evidence(self):
        body = {"model": "m", "tools": [{"name": TOOL}], "messages": [
            {"role": "user", "content": [{"type": "tool_result",
                                          "tool_use_id": fake.TOOL_USE_ID, "is_error": True,
                                          "content": "permission denied echo:" + MARKER}]}]}
        status, _ctype, data = self.post("/v1/messages", body)
        self.assertEqual(json.loads(data)["content"][0]["type"], "text")
        summary = self.state.summary()
        self.assertTrue(summary["toolResultIsError"])
        self.assertFalse(summary["toolResultHasMarker"])
        self.assertFalse(acc.fake_model_ok(summary)[0])

    def test_side_requests_get_text_and_count_tokens(self):
        status, _ctype, data = self.post("/v1/messages", {"model": "haiku", "messages": []})
        self.assertEqual(json.loads(data)["content"], [{"type": "text", "text": fake.SIDE_TEXT}])
        self.assertEqual(self.post("/v1/messages/count_tokens", {"messages": []})[0], 200)
        self.assertEqual(self.state.summary()["toolCallIssued"], 0)

    def test_responses_function_call_then_text(self):
        tools = [{"type": "function", "name": "shell"}, {"type": "function", "name": TOOL}]
        first = {"model": "neev-fake", "stream": True, "tools": tools,
                 "input": [{"type": "message", "role": "user",
                            "content": [{"type": "input_text", "text": "go"}]}]}
        _status, _ctype, body = self.post("/v1/responses", first)
        events = sse_events(body)
        done = [e for e in events if e["type"] == "response.output_item.done"]
        self.assertEqual(len(done), 1)
        call = done[0]["item"]
        self.assertEqual((call["type"], call["name"]), ("function_call", TOOL))
        self.assertEqual(json.loads(call["arguments"]), {"text": MARKER})
        self.assertEqual(events[-1]["type"], "response.completed")
        output = json.dumps({"content": [{"type": "text", "text": "echo:" + MARKER}]})
        second = dict(first, input=first["input"] + [
            call, {"type": "function_call_output", "call_id": call["call_id"], "output": output}])
        _status, _ctype, body = self.post("/v1/responses", second)
        message = [e for e in sse_events(body) if e["type"] == "response.output_item.done"][0]
        self.assertEqual(message["item"]["type"], "message")
        self.assertTrue(self.state.summary()["toolResultHasMarker"])
        _status, _ctype, data = self.post("/v1/responses", dict(first, stream=False))
        self.assertEqual(json.loads(data)["output"][0]["type"], "function_call")

    # The pinned Codex offers an MCP server's tools as ONE Responses namespace
    # tool (structure captured on the candidate image, no values).
    CODEX_NAMESPACE_TOOLS = [
        {"type": "function", "name": "shell"},
        {"type": "namespace", "name": "mcp__accept", "description": "accept tools",
         "tools": [{"type": "function", "name": "echo", "parameters": {"type": "object"}},
                   {"type": "function", "name": "secret_present",
                    "parameters": {"type": "object"}}]},
    ]

    def test_codex_namespace_tools_are_called_with_their_namespace(self):
        first = {"model": "neev-fake", "stream": True, "tools": self.CODEX_NAMESPACE_TOOLS,
                 "input": [{"type": "message", "role": "user",
                            "content": [{"type": "input_text", "text": "go"}]}]}
        _status, _ctype, body = self.post("/v1/responses", first)
        events = sse_events(body)
        done = [e for e in events if e["type"] == "response.output_item.done"]
        self.assertEqual(len(done), 1)
        call = done[0]["item"]
        self.assertEqual((call["type"], call["namespace"], call["name"]),
                         ("function_call", "mcp__accept", "echo"))
        self.assertEqual(json.loads(call["arguments"]), {"text": MARKER})
        self.assertEqual(self.state.summary()["toolCallIssued"], 1)
        # Codex answers with the output for the same call id: the marker is seen.
        output = json.dumps({"content": [{"type": "text", "text": "echo:" + MARKER}]})
        second = dict(first, input=first["input"] + [
            call, {"type": "function_call_output", "call_id": call["call_id"], "output": output}])
        _status, _ctype, body = self.post("/v1/responses", second)
        message = [e for e in sse_events(body) if e["type"] == "response.output_item.done"][0]
        self.assertEqual(message["item"]["type"], "message")
        self.assertTrue(self.state.summary()["toolResultHasMarker"])
        # Non-streaming carries the same namespaced call.
        _status, _ctype, data = self.post("/v1/responses", dict(first, stream=False))
        item = json.loads(data)["output"][0]
        self.assertEqual((item["namespace"], item["name"]), ("mcp__accept", "echo"))

    def test_namespace_parsing_is_exact(self):
        body = {"tools": self.CODEX_NAMESPACE_TOOLS}
        self.assertEqual(fake.openai_tool_names(body),
                         ["shell", "mcp__accept__echo", "mcp__accept__secret_present"])
        self.assertEqual(fake.openai_namespaces(body)["mcp__accept__echo"],
                         ("mcp__accept", "echo"))
        self.assertNotIn("shell", fake.openai_namespaces(body))
        # Malformed entries are ignored, never guessed at.
        odd = {"tools": [{"type": "namespace", "name": "mcp__x", "tools": ["echo", {"x": 1}]},
                         {"type": "namespace", "tools": [{"name": "echo"}]}, "junk"]}
        self.assertEqual(fake.openai_tool_names(odd), [])
        # A namespace for ANOTHER server never satisfies mcp__accept__echo.
        other = {"tools": [{"type": "namespace", "name": "mcp__other",
                            "tools": [{"type": "function", "name": "echo"}]}]}
        self.assertIsNone(fake.pick_tool(fake.openai_tool_names(other), TOOL))

    def test_a_flat_function_tool_is_called_without_a_namespace(self):
        items = fake.responses_items(("tool_use", TOOL), MARKER)
        self.assertNotIn("namespace", items[0])
        self.assertEqual(items[0]["name"], TOOL)

    def test_chat_completions_stream(self):
        body = {"model": "m", "stream": True, "messages": [{"role": "user", "content": "go"}],
                "tools": [{"type": "function", "function": {"name": TOOL}}]}
        _status, _ctype, data = self.post("/v1/chat/completions", body)
        chunks = sse_events(data)
        call = chunks[0]["choices"][0]["delta"]["tool_calls"][0]
        self.assertEqual(call["function"]["name"], TOOL)
        self.assertEqual(chunks[-1]["choices"][0]["finish_reason"], "tool_calls")
        body["messages"].append({"role": "tool", "tool_call_id": call["id"],
                                 "content": "echo:" + MARKER})
        _status, _ctype, data = self.post("/v1/chat/completions", dict(body, stream=False))
        self.assertEqual(json.loads(data)["choices"][0]["finish_reason"], "stop")
        self.assertTrue(self.state.summary()["toolResultHasMarker"])

    def test_models_and_unknown(self):
        conn = http.client.HTTPConnection("127.0.0.1", self.port, timeout=10)
        conn.request("GET", "/v1/models")
        self.assertEqual(json.loads(conn.getresponse().read())["data"][0]["id"], "neev-fake")
        conn.close()
        self.assertEqual(self.post("/v1/other", {"x": 1})[0], 404)

    def test_pick_tool(self):
        self.assertEqual(fake.pick_tool(["a", TOOL], TOOL), TOOL)
        self.assertEqual(fake.pick_tool(["accept.echo"], TOOL), "accept.echo")
        self.assertIsNone(fake.pick_tool(["other__echo", "accept__other"], TOOL))
        self.assertTrue(fake.carries_marker('{"content":[{"text":"echo:%s"}]}' % MARKER, MARKER))
        self.assertFalse(fake.carries_marker("prefix echo:%s" % MARKER, MARKER))


class FakeModelMainTests(unittest.TestCase):
    def run_main(self, code):
        out = io.BytesIO()
        rc = fake.main(["--port", "0", "--marker", MARKER, "--timeout", "30", "--",
                        sys.executable, "-c", code], out=out)
        return rc, out.getvalue().decode().splitlines()

    def test_forwards_child_stdout_then_one_summary(self):
        rc, lines = self.run_main("print('{\"type\":\"system\"}'); print('plain')")
        self.assertEqual(rc, 0)
        self.assertEqual(lines[:2], ['{"type":"system"}', "plain"])
        summary = json.loads(lines[-1])
        self.assertEqual(summary["type"], "neev.fake_model")
        self.assertEqual((summary["childExit"], summary["toolCallIssued"]), (0, 0))
        self.assertFalse(acc.fake_model_ok(summary)[0])

    def test_child_exit_code_propagates(self):
        rc, lines = self.run_main("import sys; sys.exit(3)")
        self.assertEqual(rc, 3)
        self.assertEqual(json.loads(lines[-1])["childExit"], 3)

    def test_marker_shape_is_enforced(self):
        with self.assertRaises(SystemExit):
            fake._parse(["--port", "1", "--marker", SECRET, "--", "true"])
        with self.assertRaises(SystemExit):
            fake._parse(["--port", "1", "--marker", MARKER])


if __name__ == "__main__":
    unittest.main()
