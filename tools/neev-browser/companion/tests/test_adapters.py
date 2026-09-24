import json
import os
import shutil
import subprocess
import sys
import tempfile
import time
import unittest

from neev_browser.adapters import (BANNED_FLAGS, AgentProcess, ProviderError,
                                   build_argv, build_mcp_config, claude_argv,
                                   codex_argv, discard_token, resolve_binary,
                                   write_launcher, write_mcp_config)
from neev_browser.runs import create_run
from neev_browser.scope import ALLOWED_TOOLS


class ArgvTest(unittest.TestCase):
    def setUp(self):
        self.base = tempfile.mkdtemp()
        os.chmod(self.base, 0o700)
        self.addCleanup(shutil.rmtree, self.base, True)
        self.paths = create_run("r1", base_dir=self.base)

    def test_claude_argv_is_scoped_and_non_interactive(self):
        argv = claude_argv("/bin/claude", self.paths, "do the thing")
        self.assertEqual(argv[0], "/bin/claude")
        self.assertIn("--print", argv)
        self.assertIn("--strict-mcp-config", argv)
        self.assertIn(self.paths.mcp_config, argv)
        self.assertIn("--no-session-persistence", argv)
        self.assertEqual(argv[argv.index("--permission-mode") + 1], "default")

    def test_claude_argv_disables_builtin_tools_and_allows_only_browser_tools(self):
        argv = claude_argv("/bin/claude", self.paths, "task")
        self.assertEqual(argv[argv.index("--tools") + 1], "")
        allowed = argv[argv.index("--allowedTools") + 1].split(",")
        self.assertEqual(
            sorted(allowed),
            sorted("mcp__neev_browser__" + name for name in ALLOWED_TOOLS))

    def test_codex_argv_is_scoped_to_the_run_and_ignores_user_config(self):
        argv = codex_argv("/bin/codex", self.paths, "task", "/run/mcp-launch.sh")
        self.assertEqual(argv[:3], ["/bin/codex", "exec", "--json"])
        self.assertIn("--ignore-user-config", argv)
        self.assertIn("--ephemeral", argv)
        self.assertEqual(argv[argv.index("--sandbox") + 1], "read-only")
        self.assertEqual(argv[argv.index("-C") + 1], self.paths.files)
        self.assertIn('mcp_servers.neev_browser.command="/run/mcp-launch.sh"', argv)

    def test_a_prompt_that_looks_like_a_flag_cannot_become_one(self):
        for argv in (claude_argv("/bin/claude", self.paths, "--sandbox"),
                     codex_argv("/bin/codex", self.paths, "--sandbox", "/l.sh")):
            self.assertEqual(argv[-2:], ["--", "--sandbox"])

    def test_no_adapter_emits_a_permission_or_sandbox_bypass_flag(self):
        for argv in (claude_argv("/bin/claude", self.paths, "t"),
                     codex_argv("/bin/codex", self.paths, "t", "/l.sh")):
            joined = " ".join(argv)
            for banned in BANNED_FLAGS:
                self.assertNotIn(banned, joined, "%s leaked into %s" % (banned, argv))

    def test_an_unknown_provider_is_refused(self):
        with self.assertRaises(ProviderError):
            build_argv("hal9000", "/bin/hal", self.paths, "t", "/l.sh")


class ScopedConfigTest(unittest.TestCase):
    def setUp(self):
        self.base = tempfile.mkdtemp()
        os.chmod(self.base, 0o700)
        self.addCleanup(shutil.rmtree, self.base, True)
        self.paths = create_run("r1", base_dir=self.base)

    def test_the_mcp_config_never_contains_the_token(self):
        launcher = write_launcher(self.paths, "s3cr3t-token", sys.executable)
        write_mcp_config(self.paths, launcher)
        with open(self.paths.mcp_config) as handle:
            blob = handle.read()
        self.assertNotIn("s3cr3t-token", blob)
        self.assertEqual(json.loads(blob)["mcpServers"]["neev_browser"]["command"],
                         launcher)

    def test_the_token_file_is_owner_only_and_removable(self):
        write_launcher(self.paths, "s3cr3t-token", sys.executable)
        token_file = os.path.join(self.paths.root, "agent-token")
        self.assertEqual(os.stat(token_file).st_mode & 0o777, 0o600)
        discard_token(self.paths)
        self.assertFalse(os.path.exists(token_file))
        self.assertTrue(os.path.exists(self.paths.root))

    def test_the_launcher_runs_and_exports_the_expected_environment(self):
        # Stand in for python3 with something that just dumps its environment.
        stub = os.path.join(self.base, "print-env")
        with open(stub, "w") as handle:
            handle.write("#!/bin/sh\nexec env\n")
        os.chmod(stub, 0o700)
        launcher = write_launcher(self.paths, "s3cr3t-token", stub)
        result = subprocess.run([launcher], stdout=subprocess.PIPE,
                                stderr=subprocess.STDOUT, timeout=30)
        printed = result.stdout.decode("utf-8", "replace")
        self.assertIn("NEEV_BROWSER_TOKEN=s3cr3t-token", printed)
        self.assertIn("NEEV_BROWSER_SOCKET=%s" % self.paths.socket, printed)

    def test_build_mcp_config_declares_exactly_one_server(self):
        self.assertEqual(list(build_mcp_config("/l.sh")["mcpServers"]), ["neev_browser"])


class ResolveBinaryTest(unittest.TestCase):
    def test_the_first_candidate_that_answers_version_wins(self):
        asked = []

        def probe(path):
            asked.append(path)
            return "codex-cli 0.154.0" if path.endswith("ChatGPT.app/Contents/Resources/codex") else None

        path, version = resolve_binary("codex", environ={"PATH": ""}, probe=probe)
        self.assertTrue(path.endswith("Resources/codex"))
        self.assertEqual(version, "codex-cli 0.154.0")

    def test_an_explicit_override_is_tried_first(self):
        environ = {"NEEV_BROWSER_CLAUDE_BIN": "/opt/claude", "PATH": ""}
        path, _ = resolve_binary("claude", environ=environ,
                                 probe=lambda p: "2.1.247" if p == "/opt/claude" else None)
        self.assertEqual(path, "/opt/claude")

    def test_a_missing_cli_fails_loudly_and_names_what_was_tried(self):
        with self.assertRaises(ProviderError) as ctx:
            resolve_binary("claude", environ={"PATH": ""}, probe=lambda p: None)
        message = str(ctx.exception)
        self.assertIn("NEEV_BROWSER_CLAUDE_BIN", message)
        self.assertIn("claude", message)

    def test_a_broken_launcher_is_skipped_rather_than_repaired(self):
        # /usr/local/bin/codex on this machine is a launcher with no platform
        # binary: it must be stepped over, not fixed.
        probes = {"/usr/local/bin/codex": None,
                  "/Applications/ChatGPT.app/Contents/Resources/codex": "codex-cli 0.154.0"}
        path, _ = resolve_binary("codex", environ={"PATH": "/usr/local/bin"},
                                 probe=lambda p: probes.get(p))
        self.assertTrue(path.startswith("/Applications/"))


class TerminationTest(unittest.TestCase):
    def test_terminate_tree_kills_the_child_and_its_grandchild(self):
        script = ("import subprocess,sys,time;"
                  "subprocess.Popen([sys.executable,'-c','import time;time.sleep(60)']);"
                  "time.sleep(60)")
        popen = subprocess.Popen([sys.executable, "-c", script], start_new_session=True)
        group = os.getpgid(popen.pid)
        AgentProcess(popen).terminate_tree(grace=1.0)
        self.assertIsNotNone(popen.poll())
        deadline = time.time() + 5
        while time.time() < deadline:
            try:
                os.killpg(group, 0)
            except OSError:
                return
            time.sleep(0.1)
        self.fail("process group survived terminate_tree")

    def test_terminating_an_already_dead_process_is_harmless(self):
        popen = subprocess.Popen([sys.executable, "-c", ""], start_new_session=True)
        popen.wait(timeout=10)
        AgentProcess(popen).terminate_tree(grace=0.2)


if __name__ == "__main__":
    unittest.main()
