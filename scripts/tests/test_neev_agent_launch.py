"""Portable tests for the production agent launch entrypoint's spec construction.
Actually spawning the sandbox is Linux-only (`neev_agent_sandbox_acceptance.py`);
these prove the CLI maps to a correct, validated SandboxSpec + egress policy."""

import io
import os
import sys
import time
import unittest
from pathlib import Path

OPERATIONS = Path(__file__).parents[1] / "operations"
sys.path.insert(0, str(OPERATIONS))

import neev_agent_launch as launch  # noqa: E402
import neev_agent_sandbox as sandbox  # noqa: E402
import neev_agent_credentials as credentials  # noqa: E402
from neev_credential_store import StoreError  # noqa: E402


def args(extra):
    base = [
        "--tenant", "org:3/user:7",
        "--slot", "a",
        "--workdir", "/var/lib/neev-native/agents/org3-user7",
        "--uid", "61000",
        "--gid", "61000",
        "--provider", "claude",
        "--credential-handle", "handle-1",
    ]
    return launch._parse(base + extra)


class LaunchSpecTests(unittest.TestCase):
    def test_deny_all_when_no_provider_ip(self):
        spec = launch.build_spec(args(["--", "claude", "-p", "hi"]))
        self.assertEqual(spec.egress.mode, "deny_all")
        self.assertEqual(spec.provider_argv, ("claude", "-p", "hi"))
        self.assertEqual(spec.run_uid, 61000)

    def test_provider_allowlist_when_ip_given(self):
        spec = launch.build_spec(
            args(["--provider-ipv4", "140.82.112.3", "--", "claude", "-p", "hi"])
        )
        self.assertEqual(spec.egress.mode, "provider_allowlist")
        self.assertEqual(spec.egress.provider_ipv4, ("140.82.112.3",))

    def test_missing_provider_argv_rejected(self):
        with self.assertRaises(sandbox.SandboxError):
            launch.build_spec(args([]))

    def test_invalid_uid_rejected(self):
        with self.assertRaises(sandbox.SandboxError):
            launch.build_spec(
                launch._parse([
                    "--tenant", "t", "--slot", "a", "--workdir", "/x/y",
                    "--uid", "0", "--gid", "0",
                    "--provider", "claude", "--credential-handle", "h1",
                    "--", "claude",
                ])
            )

    def test_malformed_provider_ip_rejected(self):
        with self.assertRaises(Exception):
            launch.build_spec(
                args(["--provider-ipv4", "not-an-ip", "--", "claude", "-p", "hi"])
            )


class WorkdirTrustTests(unittest.TestCase):
    def test_workdir_must_be_a_run_dir_directly_under_the_trusted_root(self):
        import os
        import tempfile

        with tempfile.TemporaryDirectory() as root:
            os.environ["NEEV_AGENT_WORKSPACE_ROOT"] = root
            try:
                # Accepted: a run-* dir directly under the trusted root.
                launch._validate_workdir_under_trusted_root(os.path.join(root, "run-abc123"))
                # Rejected: escaping the root, a symlinked parent, a bad name, relative.
                with self.assertRaises(sandbox.SandboxError):
                    launch._validate_workdir_under_trusted_root("/etc/cron.d")
                with self.assertRaises(sandbox.SandboxError):
                    launch._validate_workdir_under_trusted_root(os.path.join(root, "evil", "run-x"))
                with self.assertRaises(sandbox.SandboxError):
                    launch._validate_workdir_under_trusted_root(os.path.join(root, "notrun"))
                with self.assertRaises(sandbox.SandboxError):
                    launch._validate_workdir_under_trusted_root("run-abc")
                # Symlinked parent that escapes the root is rejected (realpath check).
                link = os.path.join(root, "link")
                os.symlink("/etc", link)
                with self.assertRaises(sandbox.SandboxError):
                    launch._validate_workdir_under_trusted_root(os.path.join(link, "run-x"))
            finally:
                os.environ.pop("NEEV_AGENT_WORKSPACE_ROOT", None)

    def test_a_catalogue_probe_workdir_is_accepted_and_a_bare_probe_dir_is_not(self):
        """The model-catalogue probe launches through this same validator.

        It used to ask for `probe-<id>`, which fails the `run-` rule, so every
        catalogue read died here with `invalid spec` before a sandbox, a
        credential or a process existed — and the reason went to the stderr the
        caller discards, which is why the menu only ever said the models could
        not be listed. The probe now asks for `run-probe-<id>`; this pins both
        halves so the prefix cannot be dropped again.
        """
        import os
        import tempfile

        with tempfile.TemporaryDirectory() as root:
            os.environ["NEEV_AGENT_WORKSPACE_ROOT"] = root
            try:
                launch._validate_workdir_under_trusted_root(
                    os.path.join(root, "run-probe-0123456789abcdef")
                )
                with self.assertRaises(sandbox.SandboxError):
                    launch._validate_workdir_under_trusted_root(
                        os.path.join(root, "probe-0123456789abcdef")
                    )
            finally:
                os.environ.pop("NEEV_AGENT_WORKSPACE_ROOT", None)

    def test_missing_trusted_root_env_is_rejected(self):
        os.environ.pop("NEEV_AGENT_WORKSPACE_ROOT", None)
        with self.assertRaises(sandbox.SandboxError):
            launch._validate_workdir_under_trusted_root("/data/agents/run-x")


class CredentialWiringTests(unittest.TestCase):
    def test_parse_accepts_provider_and_credential_handle(self):
        parsed = args(["--", "claude", "-p", "hi"])
        self.assertEqual(parsed.provider, "claude")
        self.assertEqual(parsed.credential_handle, "handle-1")

    def test_child_env_is_a_secret_free_allowlist(self):
        delivery = credentials.Delivery(
            home="/mnt/vol",
            env={"HOME": "/mnt/vol", "CLAUDE_CONFIG_DIR": "/mnt/vol/.claude"},
        )
        sentinels = {
            "NEEV_DATABASE_URL": "postgres://SENTINEL",
            "NEEV_ENC_KEY": "SENTINEL-enc",
            "ANTHROPIC_API_KEY": "sk-SENTINEL-a",
            "OPENAI_API_KEY": "sk-SENTINEL-o",
            "CODEX_HOME": "/host/.codex-SENTINEL",
            "GOOGLE_CLIENT_SECRET": "SENTINEL-g",
            "RESEND_API_KEY": "SENTINEL-r",
        }
        keys = list(sentinels) + ["PATH", "LANG"]
        saved = {k: os.environ.get(k) for k in keys}
        try:
            os.environ.update(sentinels)
            os.environ["PATH"] = "/usr/bin:/bin"
            os.environ["LANG"] = "C.UTF-8"
            env = launch.child_env(delivery)
        finally:
            for key, value in saved.items():
                if value is None:
                    os.environ.pop(key, None)
                else:
                    os.environ[key] = value
        self.assertEqual(env["HOME"], "/mnt/vol")
        self.assertEqual(env["CLAUDE_CONFIG_DIR"], "/mnt/vol/.claude")
        self.assertEqual(env["PATH"], "/usr/bin:/bin")
        self.assertEqual(env["LANG"], "C.UTF-8")
        for key in sentinels:
            self.assertNotIn(key, env)
        blob = "\x00".join(env.keys()) + "\x00" + "\x00".join(env.values())
        for value in sentinels.values():
            self.assertNotIn(value, blob)

    def test_child_env_injects_authoritative_slot_proxy_env(self):
        import neev_login_network_profile as network_profile

        delivery = credentials.Delivery(
            home="/mnt/vol",
            env={"HOME": "/mnt/vol", "CODEX_HOME": "/mnt/vol/.codex"},
        )
        proxy = network_profile.proxy_env("a")
        saved = {k: os.environ.get(k) for k in ("HTTPS_PROXY", "NO_PROXY")}
        try:
            # An ambient (hostile) proxy value must NOT leak; the fixed slot wins.
            os.environ["HTTPS_PROXY"] = "http://ambient-should-not-be-used:9"
            os.environ["NO_PROXY"] = "ambient.example.com"
            env = launch.child_env(delivery, proxy)
        finally:
            for key, value in saved.items():
                if value is None:
                    os.environ.pop(key, None)
                else:
                    os.environ[key] = value
        for key in ("HTTP_PROXY", "HTTPS_PROXY", "http_proxy", "https_proxy"):
            self.assertEqual(env[key], "http://10.203.0.2:3128")
        self.assertEqual(env["NO_PROXY"], "")
        self.assertEqual(env["no_proxy"], "")
        self.assertEqual(env["HOME"], "/mnt/vol")
        self.assertEqual(env["CODEX_HOME"], "/mnt/vol/.codex")

    def test_child_env_without_proxy_has_no_proxy_vars(self):
        delivery = credentials.Delivery(home="/mnt/vol", env={"HOME": "/mnt/vol"})
        env = launch.child_env(delivery)
        for key in ("HTTP_PROXY", "HTTPS_PROXY", "http_proxy", "https_proxy",
                    "NO_PROXY", "no_proxy"):
            self.assertNotIn(key, env)

    def test_main_provider_allowlist_passes_slot_proxy_env_to_child(self):
        import contextlib
        import tempfile

        captured = {}

        class FakeHandle:
            pid = os.getpid()
            stdout = None

            def wait(self):
                return 0

            def teardown(self):
                pass

        @contextlib.contextmanager
        def fake_deliver(volumes, tenant, provider, handle, uid, gid, **kwargs):
            yield credentials.Delivery(
                home="/mnt/vol",
                env={"HOME": "/mnt/vol", "CODEX_HOME": "/mnt/vol/.codex"},
            )

        def fake_launch(spec, env=None, bind_source=None, stdin_pipe=False):
            captured["env"] = env
            captured["bind_source"] = bind_source
            # A paid `exec` turn is never writable; only the catalogue probe is.
            captured["stdin_pipe"] = stdin_pipe
            return FakeHandle()

        # The runtime now owns the slot's RUN proxy through the slot_proxy CM (real one
        # spawns a namespaced listener); stub it to yield the authoritative slot env so
        # the wiring — provider+purpose scope and the env reaching the child — is what's
        # exercised here, not the netns machinery.
        import neev_login_network_profile as network_profile
        import neev_slot_proxy as slot_proxy

        @contextlib.contextmanager
        def fake_slot_proxy(provider, purpose, slot, **kwargs):
            captured["proxy_scope"] = (provider, purpose, slot)
            yield network_profile.proxy_env(slot)

        with tempfile.TemporaryDirectory() as root:
            run_dir = os.path.join(root, "run-abc")
            os.mkdir(run_dir)
            argv = [
                "--tenant", "org:3/user:7", "--slot", "b", "--workdir", run_dir,
                "--uid", "61000", "--gid", "61000", "--provider", "codex",
                "--credential-handle", "h1", "--provider-ipv4", "140.82.112.3",
                "--", "codex", "-p", "hi",
            ]
            saved_root = os.environ.get("NEEV_AGENT_WORKSPACE_ROOT")
            os.environ["NEEV_AGENT_WORKSPACE_ROOT"] = root
            saved_deliver = credentials.deliver
            saved_launch = sandbox.launch
            saved_slot_proxy = slot_proxy.slot_proxy
            credentials.deliver = fake_deliver
            sandbox.launch = fake_launch
            slot_proxy.slot_proxy = fake_slot_proxy
            try:
                rc = launch.main(argv, volumes=object())
            finally:
                credentials.deliver = saved_deliver
                sandbox.launch = saved_launch
                slot_proxy.slot_proxy = saved_slot_proxy
                if saved_root is None:
                    os.environ.pop("NEEV_AGENT_WORKSPACE_ROOT", None)
                else:
                    os.environ["NEEV_AGENT_WORKSPACE_ROOT"] = saved_root
        self.assertEqual(rc, 0)
        self.assertEqual(captured["proxy_scope"], ("codex", "run", "b"))
        # A paid turn's stdin stays DEVNULL: only the catalogue probe is written to.
        self.assertFalse(captured["stdin_pipe"])
        env = captured["env"]
        # Slot "b" proxy, routed through the fixed slot CONNECT proxy, all bypass off.
        self.assertEqual(env["HTTPS_PROXY"], "http://10.203.0.10:3128")
        self.assertEqual(env["NO_PROXY"], "")
        self.assertEqual(env["HOME"], "/mnt/vol")

    def test_cleanup_run_workdir_removes_tree_and_refuses_outside_root(self):
        import tempfile

        with tempfile.TemporaryDirectory() as root:
            saved = os.environ.get("NEEV_AGENT_WORKSPACE_ROOT")
            os.environ["NEEV_AGENT_WORKSPACE_ROOT"] = root
            try:
                run = os.path.join(root, "run-x")
                os.makedirs(os.path.join(run, "home"))
                with open(os.path.join(run, "leak"), "w") as sink:
                    sink.write("lane-cwd-file")
                with open(os.path.join(run, "home", "cred"), "w") as sink:
                    sink.write("y")
                os.symlink("/etc/passwd", os.path.join(run, "evil-link"))
                # Removes the whole run tree (incl. a symlink, without following it).
                self.assertTrue(launch._cleanup_run_workdir(run))
                self.assertFalse(os.path.exists(run))
                self.assertTrue(os.path.exists("/etc/passwd"))  # symlink target untouched
                # Refuses anything not a validated run dir under the trusted root.
                self.assertFalse(launch._cleanup_run_workdir("/etc"))
                self.assertFalse(launch._cleanup_run_workdir(os.path.join(root, "notrun")))
            finally:
                if saved is None:
                    os.environ.pop("NEEV_AGENT_WORKSPACE_ROOT", None)
                else:
                    os.environ["NEEV_AGENT_WORKSPACE_ROOT"] = saved

    def test_main_refuses_when_credentials_cannot_be_delivered(self):
        import tempfile

        class RaisingVolumes:
            def open(self, scope, handle):
                raise StoreError("unavailable")

        with tempfile.TemporaryDirectory() as root:
            run_dir = os.path.join(root, "run-abc")
            os.mkdir(run_dir)
            argv = [
                "--tenant", "org:3/user:7", "--slot", "a",
                "--workdir", run_dir, "--uid", "61000", "--gid", "61000",
                "--provider", "claude", "--credential-handle", "h1",
                "--", "claude", "-p", "hi",
            ]
            saved = os.environ.get("NEEV_AGENT_WORKSPACE_ROOT")
            os.environ["NEEV_AGENT_WORKSPACE_ROOT"] = root
            saved_lock = credentials.RUN_LOCK_DIR
            credentials.RUN_LOCK_DIR = os.path.join(root, "run-locks")
            try:
                rc = launch.main(argv, volumes=RaisingVolumes())
            finally:
                credentials.RUN_LOCK_DIR = saved_lock
                if saved is None:
                    os.environ.pop("NEEV_AGENT_WORKSPACE_ROOT", None)
                else:
                    os.environ["NEEV_AGENT_WORKSPACE_ROOT"] = saved
        # Reached credential delivery (parser + spec + workdir all valid) and
        # refused BEFORE any sandbox launch.
        self.assertEqual(rc, 5)


if __name__ == "__main__":
    unittest.main()


class _Sink:
    """A child stdin that records instead of closing out from under the test."""

    def __init__(self):
        self.written = b""
        self.closed_by_relay = False

    def write(self, chunk):
        self.written += chunk

    def flush(self):
        pass

    def close(self):
        self.closed_by_relay = True


class MetadataStdinRelayTests(unittest.TestCase):
    """The catalogue probe is the only launch that may be written to.

    `_run_child` launched with the default `stdin_pipe=False`, so the provider
    got DEVNULL. The reader writes `initialize`, waits for the reply, then sends
    `initialized` + `model/list`; against DEVNULL the app-server saw EOF at once
    and exited, and no catalogue was ever read. The relay exists for exactly that
    exchange and must not widen to anything a user pays for.
    """

    def probe_args(self, provider="codex", tail=("app-server",)):
        base = [
            "--tenant", "org:3/user:7",
            "--slot", "a",
            "--workdir", "/var/lib/neev-native/agents/run-probe-abc",
            "--uid", "61000",
            "--gid", "61000",
            "--provider", provider,
            "--credential-handle", "handle-1",
        ]
        parsed = launch._parse(base + ["--", "/usr/local/bin/codex", *tail])
        return parsed, launch.build_spec(parsed)

    def test_only_a_bare_codex_app_server_launch_is_a_metadata_probe(self):
        parsed, spec = self.probe_args()
        self.assertTrue(launch.is_metadata_probe(parsed.provider, spec))

        # A paid turn is never writable, however it is shaped.
        for tail in (
            ("exec", "--json", "--skip-git-repo-check", "hi"),
            ("exec", "--json"),
            ("app-server", "--yolo"),      # extra argument: not the bare probe
            ("login",),
        ):
            parsed, spec = self.probe_args(tail=tail)
            self.assertFalse(
                launch.is_metadata_probe(parsed.provider, spec),
                f"{tail} must not open stdin",
            )
        # Right shape, wrong provider.
        parsed, spec = self.probe_args(provider="claude")
        self.assertFalse(launch.is_metadata_probe(parsed.provider, spec))

    def test_the_relay_forwards_the_handshake_and_closes_the_childs_stdin(self):
        frames = (
            b'{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}\n'
            b'{"jsonrpc":"2.0","method":"initialized","params":{}}\n'
            b'{"jsonrpc":"2.0","id":2,"method":"model/list","params":{}}\n'
        )
        sink = _Sink()
        written = launch._relay_stdin(sink, io.BytesIO(frames))
        self.assertEqual(written, len(frames))
        self.assertEqual(sink.written, frames)
        # Closing is what lets a well-behaved app-server finish.
        self.assertTrue(sink.closed_by_relay)

    def test_the_relay_is_bounded_and_survives_a_child_that_exits_first(self):
        sink = _Sink()
        written = launch._relay_stdin(sink, io.BytesIO(b"x" * 10_000), limit=256)
        self.assertEqual(written, 256)
        self.assertEqual(len(sink.written), 256)

        class Gone(_Sink):
            def write(self, _chunk):
                raise BrokenPipeError()

        # The child's rc is the outcome; a dead pipe must not raise out of here.
        self.assertEqual(launch._relay_stdin(Gone(), io.BytesIO(b"frame\n")), 0)


class ExternallySandboxedTests(unittest.TestCase):
    """Codex must not try to sandbox itself inside our sandbox.

    Measured in the real production sandbox: user namespaces are available but
    the capability bounding set is empty, so `CAP_SYS_ADMIN` can never be held —
    Codex's bundled bubblewrap dies at `Failed to make / slave: Permission
    denied` and every model-generated command fails with it. Codex's own help
    names this case: the flag is "intended solely for running in environments
    that are externally sandboxed". That is here, and nowhere else.
    """

    def spec_for(self, provider, tail):
        parsed = launch._parse([
            "--tenant", "org:3/user:7",
            "--slot", "a",
            "--workdir", "/var/lib/neev-native/agents/run-abc",
            "--uid", "61000",
            "--gid", "61000",
            "--provider", provider,
            "--credential-handle", "handle-1",
            "--provider-ipv4", "140.82.112.3",
            "--", f"/usr/local/bin/{provider}", *tail,
        ])
        return launch.build_spec(parsed)

    def test_a_codex_exec_run_is_told_it_is_externally_sandboxed(self):
        spec = self.spec_for("codex", ["exec", "--json", "hi", "--skip-git-repo-check"])
        self.assertEqual(spec.provider_argv.count(launch._EXTERNALLY_SANDBOXED), 1)
        # Straight after the subcommand, with every other argument in order.
        self.assertEqual(spec.provider_argv[:3],
                         ("/usr/local/bin/codex", "exec", launch._EXTERNALLY_SANDBOXED))
        self.assertEqual(spec.provider_argv[3:],
                         ("--json", "hi", "--skip-git-repo-check"))

    def test_nothing_else_is_given_the_bypass(self):
        # The metadata probe runs no model-generated command, so it has no
        # nested sandbox to disable — and it must stay the recognised probe.
        probe = self.spec_for("codex", ["app-server"])
        self.assertNotIn(launch._EXTERNALLY_SANDBOXED, probe.provider_argv)
        self.assertTrue(launch.is_metadata_probe("codex", probe))
        for provider, tail in (
            ("codex", ["login"]),
            ("codex", ["app-server"]),
            ("claude", ["-p", "hi", "--output-format", "stream-json"]),
            # `exec` has to be the subcommand, not merely somewhere in the argv.
            ("claude", ["exec", "--json", "hi"]),
        ):
            with self.subTest(provider=provider, tail=tail):
                spec = self.spec_for(provider, tail)
                self.assertNotIn(launch._EXTERNALLY_SANDBOXED, spec.provider_argv)

    def test_adding_it_is_idempotent(self):
        once = launch.externally_sandboxed_argv(
            "codex", ["/usr/local/bin/codex", "exec", launch._EXTERNALLY_SANDBOXED, "hi"]
        )
        self.assertEqual(once.count(launch._EXTERNALLY_SANDBOXED), 1)

    def test_the_outer_boundary_is_untouched_by_the_bypass(self):
        """The flag disables CODEX's sandbox. Ours must be bit-for-bit the same."""
        codex = self.spec_for("codex", ["exec", "--json", "hi"])
        claude = self.spec_for("claude", ["-p", "hi"])
        for spec in (codex, claude):
            self.assertEqual(spec.run_uid, 61000)
            self.assertEqual(spec.run_gid, 61000)
            self.assertEqual(spec.egress.mode, "provider_allowlist")
            self.assertEqual(spec.egress.provider_ipv4, ("140.82.112.3",))
            self.assertEqual(spec.workdir, "/var/lib/neev-native/agents/run-abc")
            self.assertEqual(spec.tenant, "org:3/user:7")
            self.assertEqual(spec.slot, "a")
        # Same limits either way: the bypass buys the child nothing from us.
        self.assertEqual(codex.limits, claude.limits)

    def test_an_empty_netns_run_still_gets_a_deny_all_policy(self):
        parsed = launch._parse([
            "--tenant", "org:3/user:7", "--slot", "a",
            "--workdir", "/var/lib/neev-native/agents/run-abc",
            "--uid", "61000", "--gid", "61000",
            "--provider", "codex", "--credential-handle", "h",
            "--", "/usr/local/bin/codex", "exec", "--json", "hi",
        ])
        spec = launch.build_spec(parsed)
        self.assertIn(launch._EXTERNALLY_SANDBOXED, spec.provider_argv)
        self.assertEqual(spec.egress.mode, "deny_all")


class RelayWiringTests(unittest.TestCase):
    """`_run_child` must actually ASK for the pipe, not just be willing to use one.

    The bug was here: `sandbox.launch` was called with its default
    `stdin_pipe=False`, so the probe's stdin was DEVNULL and the app-server saw
    EOF before the handshake. The `exec` path already asserts the False case;
    this pins the True one, which is the half that regressed.
    """

    def run_child(self, relay_stdin):
        captured = {}
        written = []

        class Handle:
            stdout = None

            class _Stdin:
                @staticmethod
                def write(chunk):
                    written.append(chunk)

                @staticmethod
                def flush():
                    pass

                @staticmethod
                def close():
                    pass

            stdin = _Stdin()

            @staticmethod
            def wait():
                return 0

            @staticmethod
            def teardown():
                pass

            pid = os.getpid()

        def fake_launch(spec, env=None, bind_source=None, stdin_pipe=False):
            captured["stdin_pipe"] = stdin_pipe
            return Handle()

        real_launch, real_signal = launch.sandbox.launch, launch.signal.signal
        launch.sandbox.launch = fake_launch
        launch.signal.signal = lambda *_a: None  # not the main thread under unittest
        real_stdin = sys.stdin
        sys.stdin = type("S", (), {"buffer": io.BytesIO(b'{"id":1}\n')})()
        try:
            spec = launch.build_spec(launch._parse([
                "--tenant", "org:3/user:7", "--slot", "a",
                "--workdir", "/var/lib/neev-native/agents/run-probe-abc",
                "--uid", "61000", "--gid", "61000",
                "--provider", "codex", "--credential-handle", "h",
                "--", "/usr/local/bin/codex", "app-server",
            ]))
            rc = launch._run_child(spec, {}, relay_stdin=relay_stdin)
        finally:
            launch.sandbox.launch = real_launch
            launch.signal.signal = real_signal
            sys.stdin = real_stdin
        return rc, captured, written

    def test_a_probe_opens_the_pipe_and_the_handshake_reaches_the_child(self):
        rc, captured, written = self.run_child(relay_stdin=True)
        self.assertEqual(rc, 0)
        self.assertTrue(captured["stdin_pipe"], "the probe must be launched with a pipe")
        # The relay runs on a thread; give it the moment it needs to drain.
        for _ in range(200):
            if written:
                break
            time.sleep(0.01)
        self.assertEqual(b"".join(written), b'{"id":1}\n')

    def test_anything_else_is_launched_with_devnull(self):
        _rc, captured, written = self.run_child(relay_stdin=False)
        self.assertFalse(captured["stdin_pipe"])
        self.assertEqual(written, [])
