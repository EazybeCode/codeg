"""Tool-free supervision mode: launcher routing and the Claude adapter boundary.

A tool-free run is the planner/reviewer seam: text in, bounded text out, same
subscription credential, no tools, no MCP, no skills, no hooks. These tests pin
that the mode is enforced where the tools live (the provider CLI's own tool
set), that every shape the launcher does not recognise is REFUSED rather than
run as an ordinary turn, and that Codex/ACP providers are declined outright.
"""

import io
import json
import os
import stat
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

OPERATIONS = Path(__file__).parents[1] / "operations"
sys.path.insert(0, str(OPERATIONS))

import neev_agent_launch as launch  # noqa: E402
import neev_agent_sandbox as sandbox  # noqa: E402
import neev_claude_stream_adapter as adapter  # noqa: E402

ADAPTER = str(OPERATIONS / "neev_claude_stream_adapter.py")

# The exact provider argv `execution.rs::provider_argv` builds for a Claude
# tool-free run (instructions as the system prompt, fast off, explicit model).
RUST_TOOL_FREE_CLAUDE = [
    "/usr/local/bin/claude", "-p", "review this",
    "--output-format", "stream-json", "--include-partial-messages", "--verbose",
    "--append-system-prompt", "Answer only with the JSON object.",
    "--effort", "high",
    "--settings", '{"fastMode":false}',
    "--model", "claude-opus-4-1",
]


def parse(provider="claude", extra=(), argv=None):
    base = [
        "--tenant", "org:3/user:7",
        "--slot", "a",
        "--workdir", "/var/lib/neev-native/agents/run-abc",
        "--uid", "61000",
        "--gid", "61000",
        "--provider", provider,
        "--credential-handle", "handle-1",
    ]
    return launch._parse(base + list(extra) + ["--"] + list(argv or RUST_TOOL_FREE_CLAUDE))


class LauncherToolPolicyTests(unittest.TestCase):
    def test_the_policy_defaults_to_standard_so_existing_callers_are_unchanged(self):
        self.assertEqual(parse().tool_policy, "standard")

    def test_an_unknown_policy_is_refused_at_argparse(self):
        with self.assertRaises(SystemExit):
            parse(extra=["--tool-policy", "read-only"])

    def test_tool_free_claude_runs_through_the_adapter_with_tool_free_on(self):
        spec = launch.build_spec(parse(extra=["--tool-policy", "tool-free"]), style="STYLE")
        argv = list(spec.provider_argv)
        self.assertEqual(argv[:3], [sandbox.PYTHON, "-I", launch.CLAUDE_ADAPTER])
        self.assertIn("--tool-free", argv)
        self.assertNotIn("--enable-steering", argv)
        self.assertEqual(argv[-2:], ["--", "review this"])
        # Every chosen setting survives the rewrite.
        for pair in (["--model", "claude-opus-4-1"], ["--effort", "high"],
                     ["--settings", '{"fastMode":false}'],
                     ["--append-system-prompt", "Answer only with the JSON object."]):
            index = argv.index(pair[0])
            self.assertEqual(argv[index:index + 2], pair)

    def test_tool_free_does_not_append_the_reader_style_policy(self):
        spec = launch.build_spec(parse(extra=["--tool-policy", "tool-free"]), style="STYLE")
        self.assertNotIn("STYLE", " ".join(spec.provider_argv))

    def test_an_unknown_claude_option_is_refused_not_passed_through_raw(self):
        argv = RUST_TOOL_FREE_CLAUDE + ["--mcp-config", "/tmp/x.json"]
        with self.assertRaises(sandbox.SandboxError) as caught:
            launch.build_spec(parse(extra=["--tool-policy", "tool-free"], argv=argv), style="")
        self.assertEqual(caught.exception.category, "invalid")

    def test_a_non_canonical_claude_shape_is_refused(self):
        argv = ["/usr/local/bin/claude", "--print", "hi"]
        with self.assertRaises(sandbox.SandboxError):
            launch.build_spec(parse(extra=["--tool-policy", "tool-free"], argv=argv), style="")

    def test_steering_cannot_be_combined_with_tool_free(self):
        with self.assertRaises(sandbox.SandboxError):
            launch.build_spec(
                parse(extra=["--tool-policy", "tool-free", "--enable-steering"]), style="")

    def test_codex_is_declined_in_both_transports(self):
        codex = ["/usr/local/bin/codex", "exec", "--json", "hi", "--skip-git-repo-check"]
        for stream in ("exec", "app-server"):
            with self.subTest(stream=stream):
                with self.assertRaises(sandbox.SandboxError) as caught:
                    launch.build_spec(parse(
                        provider="codex",
                        extra=["--tool-policy", "tool-free", "--codex-stream", stream],
                        argv=codex), style="")
                self.assertEqual(caught.exception.category, "unsupported")

    def test_acp_providers_are_declined(self):
        argv = ["/usr/local/bin/gemini", "--neev-acp", "--", "hi"]
        with self.assertRaises(sandbox.SandboxError) as caught:
            launch.build_spec(parse(provider="gemini", extra=["--tool-policy", "tool-free"],
                                    argv=argv), style="")
        self.assertEqual(caught.exception.category, "unsupported")

    def test_standard_claude_routing_is_unchanged(self):
        spec = launch.build_spec(parse(extra=["--enable-steering"]), style="")
        self.assertNotIn("--tool-free", spec.provider_argv)
        self.assertIn("--enable-steering", spec.provider_argv)

    def test_the_prompt_is_never_read_as_a_tool_free_option(self):
        argv = list(RUST_TOOL_FREE_CLAUDE)
        argv[2] = "--mcp-config"
        spec = launch.build_spec(parse(extra=["--tool-policy", "tool-free"], argv=argv), style="")
        self.assertEqual(list(spec.provider_argv)[-2:], ["--", "--mcp-config"])


class AdapterArgvTests(unittest.TestCase):
    def parse(self, extra):
        return adapter._parse(["--claude", "/usr/local/bin/claude"] + extra + ["--", "hi"])

    def test_tool_free_adds_every_provider_side_switch(self):
        argv = adapter.claude_argv(self.parse(["--tool-free"]))
        index = argv.index("--tools")
        self.assertEqual(argv[index + 1], "")
        for flag in ("--strict-mcp-config", "--disable-slash-commands", "--safe-mode",
                     "--no-session-persistence"):
            self.assertIn(flag, argv)
        index = argv.index("--permission-mode")
        self.assertEqual(argv[index + 1], "dontAsk")
        self.assertNotIn("--mcp-config", argv)

    def test_ordinary_runs_get_none_of_it(self):
        argv = adapter.claude_argv(self.parse([]))
        for flag in ("--tools", "--strict-mcp-config", "--safe-mode", "--permission-mode"):
            self.assertNotIn(flag, argv)


class AdapterToolFreeEventTests(unittest.TestCase):
    def setUp(self):
        self.output = io.StringIO()
        self.emit = adapter.Emitter(self.output)
        self.state = adapter.SteerState(self.emit)
        self.state.tool_free = True

    def handle(self, event):
        return adapter.handle_event(event, json.dumps(event), self.emit, self.state)

    def failure(self):
        lines = [json.loads(line) for line in self.output.getvalue().splitlines() if line]
        return lines[-1]

    def test_an_init_with_no_tools_and_no_mcp_is_accepted(self):
        self.assertIsNone(self.handle(
            {"type": "system", "subtype": "init", "tools": [], "mcp_servers": []}))

    def test_an_init_that_still_declares_tools_fails_closed(self):
        self.assertEqual(self.handle(
            {"type": "system", "subtype": "init", "tools": ["Bash"], "mcp_servers": []}), 4)
        self.assertEqual(self.failure()["code"], "tool_free_unenforced")

    def test_an_init_with_mcp_servers_fails_closed(self):
        self.assertEqual(self.handle({"type": "system", "subtype": "init", "tools": [],
                                      "mcp_servers": [{"name": "x"}]}), 4)

    def test_an_init_that_does_not_report_its_tools_fails_closed(self):
        self.assertEqual(self.handle({"type": "system", "subtype": "init"}), 4)

    def test_any_tool_use_block_fails_the_run(self):
        for event in (
            {"type": "stream_event", "event": {"type": "content_block_start", "index": 1,
                                               "content_block": {"type": "tool_use"}}},
            {"type": "stream_event", "event": {"type": "content_block_start", "index": 1,
                                               "content_block": {"type": "server_tool_use"}}},
            {"type": "assistant", "message": {"content": [{"type": "mcp_tool_use"}]}},
            {"type": "user", "message": {"content": [{"type": "tool_result"}]}},
        ):
            with self.subTest(event=event):
                self.output.truncate(0)
                self.output.seek(0)
                self.assertEqual(self.handle(event), 4)
                self.assertEqual(self.failure()["code"], "tool_use")

    def test_ordinary_text_passes(self):
        self.assertIsNone(self.handle({"type": "assistant",
                                       "message": {"content": [{"type": "text", "text": "ok"}]}}))


FAKE = r'''#!/usr/bin/env python3
import json, os, sys
argv = sys.argv[1:]
open(os.environ["ARGV_OUT"], "w").write(json.dumps(argv))
for line in sys.stdin:
    if line.strip():
        break
def send(p):
    sys.stdout.write(json.dumps(p) + "\n"); sys.stdout.flush()
tools = [] if "--tools" in argv and argv[argv.index("--tools") + 1] == "" else ["Bash"]
send({"type": "system", "subtype": "init", "tools": tools, "mcp_servers": []})
send({"type": "assistant", "message": {"content": [{"type": "text", "text": "{}"}]}})
send({"type": "result", "subtype": "success", "is_error": False, "result": "{}"})
'''


class AdapterEndToEndTests(unittest.TestCase):
    def run_adapter(self, extra):
        with tempfile.TemporaryDirectory() as directory:
            fake = Path(directory) / "claude"
            fake.write_text(FAKE)
            fake.chmod(fake.stat().st_mode | stat.S_IEXEC)
            out = Path(directory) / "argv.json"
            process = subprocess.run(
                [sys.executable, ADAPTER, "--claude", str(fake), "--cwd", directory,
                 *extra, "--", "hello"],
                capture_output=True, text=True, timeout=30,
                env=dict(os.environ, ARGV_OUT=str(out)),
            )
            seen = json.loads(out.read_text()) if out.exists() else None
            return process.returncode, process.stdout, seen

    def test_a_tool_free_turn_completes_with_tools_off_at_the_cli(self):
        code, stdout, seen = self.run_adapter(["--tool-free"])
        self.assertEqual(code, 0, stdout)
        self.assertEqual(seen[seen.index("--tools") + 1], "")

    def test_steering_and_tool_free_are_refused_together(self):
        code, stdout, seen = self.run_adapter(["--tool-free", "--enable-steering"])
        self.assertEqual(code, 2)
        self.assertIsNone(seen)  # the provider was never started


if __name__ == "__main__":
    unittest.main()
