"""Launcher wiring for shared integrations.

The bundle (servers + secrets) arrives as the FIRST line of the launcher's stdin,
never argv; its hosts reach the slot proxy; the broker sidecar is started in the
run's netns under its own uid; steering lines that follow the bundle still reach
the provider; the admin's integration check runs without any credential volume."""

import contextlib
import io
import json
import os
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "operations"))

import neev_agent_credentials as credentials  # noqa: E402
import neev_agent_launch as launch  # noqa: E402
import neev_agent_sandbox as sandbox  # noqa: E402
import neev_login_network_profile as network_profile  # noqa: E402
import neev_slot_proxy as slot_proxy  # noqa: E402

SECRET = "SYNTHETIC-SECRET-VALUE-4f9a2c"
BUNDLE = {"neevIntegrations": 1, "servers": [
    {"key": "fireflies", "transport": "remote", "url": "https://api.fireflies.ai/mcp",
     "hosts": ["api.fireflies.ai"], "env": {}, "headers": {"Authorization": "Bearer " + SECRET}},
    {"key": "hubspot", "transport": "package", "package": "hubspot",
     "hosts": ["api.hubspot.com"], "env": {"PRIVATE_APP_ACCESS_TOKEN": SECRET}, "headers": {}},
]}
BUNDLE_LINE = (json.dumps(BUNDLE) + "\n").encode()


class FakeHandle:
    pid = os.getpid()
    stdout = None
    stdin = None
    netns = "neev-agent-b"

    def wait(self):
        return 0

    def teardown(self):
        pass


class Harness(unittest.TestCase):
    def run_main(self, argv_tail, stdin_bytes, *, check=False, launch_fails=False,
                 stale=False, broker_clean=True):
        captured = {"brokers": [], "order": [], "quarantined": []}

        @contextlib.contextmanager
        def fake_deliver(volumes, tenant, provider, handle, uid, gid, **kwargs):
            captured["delivered"] = provider
            yield credentials.Delivery(home="/mnt/vol", env={"HOME": "/mnt/vol"})

        def fake_launch(spec, env=None, bind_source=None, stdin_pipe=False,
                        before_spawn=None):
            captured["spec"] = spec
            captured["env"] = env
            captured["stdin_pipe"] = stdin_pipe
            if before_spawn is not None:
                # The real launch wires the netns, calls this, THEN spawns.
                before_spawn(FakeHandle.netns)
            if launch_fails:
                raise launch.sandbox.SandboxError("launch")
            captured["order"].append("spawn")
            return FakeHandle()

        @contextlib.contextmanager
        def fake_slot_proxy(provider, purpose, slot, **kwargs):
            captured["proxy"] = (provider, purpose, slot, tuple(kwargs.get("extra_hosts", ())))
            yield network_profile.proxy_env(slot)

        class FakeBroker:
            def stop(self):
                captured["order"].append("broker-stop")
                return broker_clean

        def fake_start_broker(**kwargs):
            captured["brokers"].append(kwargs)
            captured["order"].append("broker-start")
            return FakeBroker()

        reader = launch._FdReader.__new__(launch._FdReader)
        reader._pending = bytearray(stdin_bytes)
        reader._eof = True
        reader._fd = None

        root = tempfile.mkdtemp()
        run_dir = os.path.join(root, "run-abc")
        os.mkdir(run_dir)
        argv = ["--tenant", "org:3/user:7", "--slot", "b", "--workdir", run_dir,
                "--uid", "61001", "--gid", "61001"] + argv_tail
        # Patch the module objects the launcher resolves at call time: other
        # suites may have replaced entries in sys.modules since import.
        import importlib

        live_proxy = importlib.import_module("neev_slot_proxy")
        live_credentials, live_sandbox = launch.credentials, launch.sandbox
        saved = (live_credentials.deliver, live_sandbox.launch, live_proxy.slot_proxy,
                 launch._stdin_reader, launch.start_broker, os.environ.get("NEEV_AGENT_WORKSPACE_ROOT"))
        live_credentials.deliver = fake_deliver
        live_sandbox.launch = fake_launch
        live_proxy.slot_proxy = fake_slot_proxy
        launch._stdin_reader = lambda: reader
        launch.start_broker = fake_start_broker
        saved_stale, saved_quarantine = launch.stale_broker, live_proxy.quarantine
        saved_reclaim = launch.reclaim_slot
        launch.stale_broker = lambda slot: stale
        launch.reclaim_slot = lambda slot, **kw: True
        live_proxy.quarantine = lambda slot, **kw: captured["quarantined"].append(slot)
        os.environ["NEEV_AGENT_WORKSPACE_ROOT"] = root
        out = io.StringIO()
        try:
            with contextlib.redirect_stderr(out):
                captured["rc"] = launch.main(argv, volumes=object())
        finally:
            (live_credentials.deliver, live_sandbox.launch, live_proxy.slot_proxy,
             launch._stdin_reader, launch.start_broker, env_root) = saved
            launch.stale_broker, live_proxy.quarantine = saved_stale, saved_quarantine
            launch.reclaim_slot = saved_reclaim
            if env_root is None:
                os.environ.pop("NEEV_AGENT_WORKSPACE_ROOT", None)
            else:
                os.environ["NEEV_AGENT_WORKSPACE_ROOT"] = env_root
        captured["stderr"] = out.getvalue()
        captured["reader"] = reader
        return captured


class RunWithIntegrationsTest(Harness):
    RUN = ["--provider", "claude", "--credential-handle", "h1", "--provider-ipv4",
           "160.79.104.10", "--integrations-stdin", "--", "claude", "-p", "hi",
           "--output-format", "stream-json"]

    def test_bundle_hosts_reach_the_proxy_and_the_broker_gets_the_bundle(self):
        got = self.run_main(self.RUN, BUNDLE_LINE)
        self.assertEqual(got["rc"], 0, got["stderr"])
        self.assertEqual(got["proxy"], ("claude", "run", "b",
                                        ("api.fireflies.ai", "api.hubspot.com")))
        broker = got["brokers"][0]
        self.assertEqual(broker["netns"], "neev-agent-b")
        self.assertEqual(broker["slot"], "b")
        self.assertEqual(broker["peer_uid"], 61001)
        self.assertEqual(json.loads(broker["bundle"])["servers"][0]["key"], "fireflies")

    def test_the_broker_is_ready_before_the_agent_and_stopped_after_it(self):
        got = self.run_main(self.RUN, BUNDLE_LINE)
        self.assertEqual(got["order"], ["broker-start", "spawn", "broker-stop"])

    def test_a_failed_spawn_still_stops_the_broker(self):
        got = self.run_main(self.RUN, BUNDLE_LINE, launch_fails=True)
        self.assertEqual(got["rc"], 3)
        self.assertEqual(got["order"], ["broker-start", "broker-stop"])

    def test_a_stale_broker_quarantines_the_slot_and_runs_nothing(self):
        got = self.run_main(self.RUN, BUNDLE_LINE, stale=True)
        self.assertEqual(got["rc"], 7)
        self.assertEqual(got["quarantined"], ["b"])
        self.assertNotIn("spec", got)
        self.assertIn("stale integration broker", got["stderr"])

    def test_unverified_broker_cleanup_quarantines_the_slot(self):
        got = self.run_main(self.RUN, BUNDLE_LINE, broker_clean=False)
        self.assertEqual(got["rc"], 6)
        # (main's existing rc-6 path quarantines again; quarantine is idempotent)
        self.assertEqual(set(got["quarantined"]), {"b"})
        self.assertIn("cleanup unverified", got["stderr"])

    def test_unverified_cleanup_after_a_failed_spawn_also_quarantines(self):
        got = self.run_main(self.RUN, BUNDLE_LINE, launch_fails=True, broker_clean=False)
        self.assertEqual(got["rc"], 6)
        self.assertEqual(set(got["quarantined"]), {"b"})

    def test_accept_fixture_requires_the_bundle_flag(self):
        with self.assertRaises(SystemExit):
            launch._parse(["--tenant", "t", "--slot", "a", "--workdir", "/x", "--uid", "61000",
                           "--gid", "61000", "--integration-check", "--accept-fixture"])

    def test_no_secret_reaches_the_agent_spec_or_env(self):
        got = self.run_main(self.RUN, BUNDLE_LINE)
        self.assertNotIn(SECRET, json.dumps(list(got["spec"].provider_argv)))
        self.assertNotIn(SECRET, json.dumps(got["env"]))
        self.assertNotIn(SECRET, got["stderr"])

    def test_an_invalid_bundle_refuses_before_any_proxy(self):
        got = self.run_main(self.RUN, b'{"neevIntegrations":1,"servers":[{"key":"Bad"}]}\n')
        self.assertEqual(got["rc"], 2)
        self.assertNotIn("proxy", got)
        self.assertIn("integrations", got["stderr"])

    def test_steering_after_the_bundle_still_reads_from_the_same_stdin(self):
        steer = b'{"requestId":"r1","text":"go"}\n'
        got = self.run_main(self.RUN, BUNDLE_LINE + steer)
        self.assertEqual(got["rc"], 0)
        self.assertEqual(bytes(got["reader"]._pending), steer)

    def test_without_the_flag_nothing_is_read_and_no_hosts_are_added(self):
        argv = [a for a in self.RUN if a != "--integrations-stdin"]
        got = self.run_main(argv, BUNDLE_LINE)
        self.assertEqual(got["proxy"], ("claude", "run", "b", ()))
        self.assertEqual(got["brokers"], [])
        self.assertEqual(bytes(got["reader"]._pending), BUNDLE_LINE)


class CheckModeTest(Harness):
    def test_check_runs_the_fixed_checker_without_credentials(self):
        got = self.run_main(["--integration-check", "--integrations-stdin",
                             "--provider-ipv4", "160.79.104.10"], BUNDLE_LINE)
        self.assertEqual(got["rc"], 0, got["stderr"])
        self.assertNotIn("delivered", got)
        self.assertEqual(got["proxy"], ("none", "integrations", "b",
                                        ("api.fireflies.ai", "api.hubspot.com")))
        argv = list(got["spec"].provider_argv)
        self.assertEqual(argv[:3], [sandbox.PYTHON, "-I", launch.CHECKER])
        self.assertEqual(argv[3:], ["fireflies", "hubspot"])
        self.assertEqual(got["env"]["HOME"], got["spec"].workdir)
        self.assertEqual(got["env"]["HTTPS_PROXY"], "http://10.203.0.10:3128")
        self.assertNotIn(SECRET, json.dumps(got["env"]))

    def test_check_requires_a_bundle(self):
        got = self.run_main(["--integration-check"], b"")
        self.assertEqual(got["rc"], 2)

    def test_a_normal_run_still_requires_provider_and_handle(self):
        with self.assertRaises(SystemExit):
            launch._parse(["--tenant", "t", "--slot", "a", "--workdir", "/x", "--uid", "61000",
                           "--gid", "61000", "--", "claude", "-p", "hi"])


class BrokerArgvTest(unittest.TestCase):
    def test_broker_runs_as_its_own_uid_in_the_run_netns(self):
        argv = launch.broker_launch_argv(netns="neev-agent-a", slot="a", tenant="org:1/user:2",
                                         peer_uid=61000, broker_dir="/run/neev-mcp/a")
        self.assertEqual(argv[:4], [sandbox.IP, "netns", "exec", "neev-agent-a"])
        self.assertIn("--reuid=%d" % launch.BROKER_UIDS["a"], argv)
        self.assertIn("--no-new-privs", argv)
        self.assertIn("--bounding-set=-all", argv)
        tail = argv[argv.index(launch.BROKER) - 2:]
        self.assertEqual(tail, [sandbox.PYTHON, "-I", launch.BROKER, "--peer-uid", "61000",
                                "--home", "/run/neev-mcp/a"])
        self.assertNotEqual(launch.BROKER_UIDS["a"], 61000)
        self.assertEqual(len(set(launch.BROKER_UIDS.values())), 2)
        self.assertTrue(set(launch.BROKER_UIDS.values()).isdisjoint({61000, 61001, 62000, 65532}))

    def test_broker_refuses_an_unnamed_netns(self):
        with self.assertRaises(sandbox.SandboxError):
            launch.broker_launch_argv(netns=None, slot="a", tenant="t", peer_uid=61000,
                                      broker_dir="/run/neev-mcp/a")


class ClaudeMcpArgvTest(unittest.TestCase):
    BASE = ["claude", "-p", "hi", "--output-format", "stream-json", "--verbose",
            "--mcp-config", "/data/agents/run-x/neev-library/mcp.json",
            "--allowedTools", "mcp__github,mcp__fireflies"]

    def test_streaming_adapter_keeps_mcp_config_and_allowed_tools(self):
        argv = launch.claude_stream_argv("claude", self.BASE, enable_steering=True)
        self.assertIn(launch.CLAUDE_ADAPTER, argv)
        i = argv.index("--mcp-config")
        self.assertEqual(argv[i + 1], "/data/agents/run-x/neev-library/mcp.json")
        j = argv.index("--allowedTools")
        self.assertEqual(argv[j + 1], "mcp__github,mcp__fireflies")
        self.assertLess(j, argv.index("--"))

    def test_style_policy_still_applies_with_mcp(self):
        argv = launch.response_style_argv("claude", self.BASE, "STYLE")
        self.assertEqual(argv[-2:], ["--append-system-prompt", "STYLE"])

    def test_tool_free_refuses_mcp(self):
        with self.assertRaises(sandbox.SandboxError):
            launch.tool_free_argv("claude", self.BASE)

    def test_adapter_passes_them_to_the_cli(self):
        import neev_claude_stream_adapter as adapter

        args = adapter._parse(["--claude", "/usr/local/bin/claude", "--mcp-config", "/x/mcp.json",
                               "--allowedTools", "mcp__github", "--", "hi"])
        argv = adapter.claude_argv(args)
        self.assertEqual(argv[argv.index("--mcp-config") + 1], "/x/mcp.json")
        self.assertEqual(argv[argv.index("--allowedTools") + 1], "mcp__github")

    def test_adapter_tool_free_never_carries_mcp(self):
        import neev_claude_stream_adapter as adapter

        args = adapter._parse(["--claude", "/c", "--tool-free", "--mcp-config", "/x/mcp.json",
                               "--", "hi"])
        with self.assertRaises(SystemExit):
            adapter.claude_argv(args)


if __name__ == "__main__":
    unittest.main()


class BrokerHygieneTest(unittest.TestCase):
    def test_sweep_removes_only_the_given_uids_entries(self):
        root = tempfile.mkdtemp()
        mine = os.path.join(root, "node-cache")
        os.mkdir(mine)
        open(os.path.join(mine, "x"), "w").close()
        open(os.path.join(root, "f"), "w").close()
        link = os.path.join(root, "link")
        os.symlink("/etc/passwd", link)
        # Another uid: nothing of ours is touched.
        self.assertEqual(launch.sweep_uid_temp(os.getuid() + 1, roots=(root,)), 0)
        self.assertTrue(os.path.exists(mine))
        removed = launch.sweep_uid_temp(os.getuid(), roots=(root,))
        self.assertEqual(removed, 3)
        self.assertEqual(os.listdir(root), [])
        self.assertTrue(os.path.exists("/etc/passwd"))

    def test_uid_processes_reads_proc_status(self):
        proc = tempfile.mkdtemp()
        for pid, uid in (("12", 63001), ("13", 61001), ("self", 63001)):
            os.mkdir(os.path.join(proc, pid))
            with open(os.path.join(proc, pid, "status"), "w") as handle:
                handle.write("Name:\tx\nUid:\t%d\t%d\t%d\t%d\n" % (uid, uid, uid, uid))
        self.assertEqual(launch.uid_processes(63001, proc=proc), [12])
        self.assertTrue(launch.stale_broker("b", proc=proc))
        self.assertFalse(launch.stale_broker("a", proc=proc))
        self.assertEqual(launch.uid_processes(63001, proc="/nonexistent"), [])


class FailClosedHygieneTest(unittest.TestCase):
    def test_reclaim_verifies_processes_dir_and_temp(self):
        proc = tempfile.mkdtemp()
        roots = (tempfile.mkdtemp(),)
        run_root = tempfile.mkdtemp()
        # clean slot
        self.assertTrue(launch.reclaim_slot("a", proc=proc, roots=roots, run_root=run_root))
        # a live broker-uid process -> not clean
        os.mkdir(os.path.join(proc, "42"))
        with open(os.path.join(proc, "42", "status"), "w") as handle:
            handle.write("Uid:\t63000\t63000\t63000\t63000\n")
        self.assertFalse(launch.reclaim_slot("a", proc=proc, roots=roots, run_root=run_root))
        self.assertTrue(launch.reclaim_slot("b", proc=proc, roots=roots, run_root=run_root))

    def test_leftovers_are_found_two_levels_into_foreign_dirs(self):
        root = tempfile.mkdtemp()
        nested = os.path.join(root, "foreign", "deeper")
        os.makedirs(nested)
        open(os.path.join(nested, "mine"), "w").close()
        # Everything here is ours, so the owned top-level dir is what is reported
        # (removing it removes what is inside).
        found = launch.leftover_uid_temp(os.getuid(), roots=(root,))
        self.assertEqual(found, [os.path.join(root, "foreign")])
        self.assertEqual(launch.leftover_uid_temp(os.getuid() + 1, roots=(root,)), [])

    def test_remove_broker_dir_reports_whether_it_is_gone(self):
        base = tempfile.mkdtemp()
        path = os.path.join(base, "a")
        os.makedirs(os.path.join(path, "x"))
        self.assertTrue(launch._remove_broker_dir(path))
        self.assertTrue(launch._remove_broker_dir(path))  # already gone is gone
        os.symlink("/etc", path)
        self.assertTrue(launch._remove_broker_dir(path))
        self.assertTrue(os.path.exists("/etc"))


class BundleFirewallTest(unittest.TestCase):
    def test_steering_relay_drops_a_bundle_line_and_aborts(self):
        seen = []
        source = io.BytesIO(b'{"requestId":"r1","text":"hi"}\n' + BUNDLE_LINE
                            + b'{"requestId":"r2","text":"after"}\n')
        reader = launch._FdReader.__new__(launch._FdReader)
        reader._pending, reader._eof, reader._fd = bytearray(source.getvalue()), True, None
        sink = io.BytesIO()
        sink.close = lambda: None
        launch._relay_steering(sink, reader, on_bundle=lambda: seen.append(1))
        self.assertEqual(seen, [1])
        self.assertEqual(sink.getvalue(), b'{"requestId":"r1","text":"hi"}\n')
        self.assertNotIn(SECRET.encode(), sink.getvalue())

    def test_probe_relay_never_forwards_a_bundle(self):
        seen = []
        sink = io.BytesIO()
        sink.close = lambda: None
        launch._relay_stdin(sink, io.BytesIO(BUNDLE_LINE), on_bundle=lambda: seen.append(1))
        self.assertEqual(seen, [1])
        self.assertEqual(sink.getvalue(), b"")


class SingleGrantTest(unittest.TestCase):
    TWICE = ["claude", "-p", "hi", "--output-format", "stream-json",
             "--allowedTools", "mcp__a", "--allowedTools", "mcp__b"]

    def test_streaming_rewrite_refuses_a_repeated_grant(self):
        with self.assertRaises(sandbox.SandboxError):
            launch.claude_stream_argv("claude", self.TWICE, enable_steering=True)

    def test_raw_path_refuses_a_repeated_grant_or_config(self):
        args = launch._parse(["--tenant", "org:1/user:2", "--slot", "a",
                              "--workdir", "/data/agents/run-x", "--uid", "61000",
                              "--gid", "61000", "--provider", "claude",
                              "--credential-handle", "h", "--"] + self.TWICE)
        with self.assertRaises(sandbox.SandboxError):
            launch.build_spec(args, style="")
        twice_config = ["claude", "-p", "hi", "--mcp-config", "/a", "--mcp-config", "/b"]
        args.provider_argv = ["--"] + twice_config
        with self.assertRaises(sandbox.SandboxError):
            launch.build_spec(args, style="")

    def test_adapter_refuses_a_repeated_grant(self):
        import neev_claude_stream_adapter as adapter

        with self.assertRaises(SystemExit):
            adapter._parse(["--claude", "/c", "--allowedTools", "mcp__a",
                            "--allowedTools", "mcp__b", "--", "hi"])


class FixtureBundleTest(unittest.TestCase):
    def test_the_fixture_package_is_accepted_only_with_the_flag_and_a_valid_fixture(self):
        import neev_mcp_broker as mcp_broker

        line = (json.dumps({"neevIntegrations": 1, "servers": [
            {"key": "accept", "transport": "package", "package": "accept-stub",
             "hosts": ["api.example.com"], "headers": {},
             "env": {"NEEV_ACCEPT_TOKEN": "neev-syn-1234"}}]}) + "\n").encode()

        def reader():
            r = launch._FdReader.__new__(launch._FdReader)
            r._pending, r._eof, r._fd = bytearray(line), True, None
            return r

        with self.assertRaises(sandbox.SandboxError):
            launch.read_integrations(reader())
        with self.assertRaises(sandbox.SandboxError):
            launch.read_integrations(reader(), accept_fixture=True)  # no fixture on disk
        spec = mcp_broker.PackageSpec(argv=("/usr/bin/python3", "-I", "/opt/neev-accept/s.py"),
                                      env_names=("NEEV_ACCEPT_TOKEN",))
        got = launch.read_integrations(reader(), accept_fixture=True, fixture_spec=spec)
        self.assertTrue(got.accept_fixture)
        argv = launch.broker_launch_argv("neev-agent-a", "a", "t", 61000, "/run/neev-mcp/a",
                                         accept_fixture=True)
        self.assertEqual(argv[-1], "--accept-fixture")


class ResidualFixesTest(unittest.TestCase):
    def test_a_marker_split_across_probe_chunks_is_still_caught(self):
        seen = []
        payload = b"x" * (4096 - 5) + BUNDLE_LINE  # marker straddles the 4096 boundary

        class Chunky:
            def __init__(self, data):
                self.data = data

            def read1(self, n):
                out, self.data = self.data[:n], self.data[n:]
                return out

        sink = io.BytesIO()
        sink.close = lambda: None
        launch._relay_stdin(sink, Chunky(payload), on_bundle=lambda: seen.append(1))
        self.assertEqual(seen, [1])
        self.assertNotIn(SECRET.encode(), sink.getvalue())

    def test_accept_fixture_is_check_mode_only(self):
        base = ["--tenant", "t", "--slot", "a", "--workdir", "/x", "--uid", "61000",
                "--gid", "61000"]
        with self.assertRaises(SystemExit):
            launch._parse(base + ["--provider", "claude", "--credential-handle", "h",
                                  "--integrations-stdin", "--accept-fixture", "--",
                                  "claude", "-p", "hi"])
        args = launch._parse(base + ["--integration-check", "--integrations-stdin",
                                     "--accept-fixture"])
        self.assertTrue(args.accept_fixture)
