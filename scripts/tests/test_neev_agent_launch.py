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

    def test_main_forwards_the_metadata_probe_all_the_way_to_the_pipe(self):
        """`_deliver_run_cleanup` must pass the probe verdict through.

        The unit test proves `_run_child(relay_stdin=True)` opens the pipe and
        the argv test proves `app-server` is recognised, but nothing joined
        them: a `main` that never forwarded the flag would satisfy both and
        still hand the app-server a DEVNULL stdin, which is exactly the bug.
        """
        import contextlib
        import tempfile

        captured = {}

        class FakeHandle:
            pid = os.getpid()
            stdout = None
            stdin = None

            @staticmethod
            def wait():
                return 0

            @staticmethod
            def teardown():
                pass

        @contextlib.contextmanager
        def fake_deliver(*_a, **_k):
            yield credentials.Delivery(home="/mnt/vol", env={"HOME": "/mnt/vol"})

        def fake_launch(spec, env=None, bind_source=None, stdin_pipe=False):
            captured["stdin_pipe"] = stdin_pipe
            captured["argv"] = spec.provider_argv
            return FakeHandle()

        import neev_login_network_profile as network_profile
        import neev_slot_proxy as slot_proxy

        @contextlib.contextmanager
        def fake_slot_proxy(provider, purpose, slot, **kwargs):
            yield network_profile.proxy_env(slot)

        with tempfile.TemporaryDirectory() as root:
            run_dir = os.path.join(root, "run-probe-abc")
            os.mkdir(run_dir)
            argv = [
                "--tenant", "org:3/user:7", "--slot", "b", "--workdir", run_dir,
                "--uid", "61000", "--gid", "61000", "--provider", "codex",
                "--credential-handle", "h1",
                "--", "/usr/local/bin/codex", "app-server",
            ]
            saved_root = os.environ.get("NEEV_AGENT_WORKSPACE_ROOT")
            os.environ["NEEV_AGENT_WORKSPACE_ROOT"] = root
            saved = (credentials.deliver, sandbox.launch, slot_proxy.slot_proxy)
            credentials.deliver = fake_deliver
            sandbox.launch = fake_launch
            slot_proxy.slot_proxy = fake_slot_proxy
            try:
                rc = launch.main(argv, volumes=object())
            finally:
                credentials.deliver, sandbox.launch, slot_proxy.slot_proxy = saved
                if saved_root is None:
                    os.environ.pop("NEEV_AGENT_WORKSPACE_ROOT", None)
                else:
                    os.environ["NEEV_AGENT_WORKSPACE_ROOT"] = saved_root
        self.assertEqual(rc, 0)
        self.assertTrue(
            captured["stdin_pipe"],
            "main must forward is_metadata_probe so the probe gets a pipe",
        )
        self.assertEqual(captured["argv"], ("/usr/local/bin/codex", "app-server"))

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
        # Straight after the subcommand, with every option in order and the
        # prompt moved behind the separator (see PromptArgvBoundaryTests).
        self.assertEqual(spec.provider_argv, (
            "/usr/local/bin/codex", "exec", launch._EXTERNALLY_SANDBOXED,
            "--json", "--skip-git-repo-check", "--", "hi",
        ))

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
        # The bypass buys the child nothing from us. Address space is the one
        # deliberate difference and it belongs to the V8 reservation, not to
        # this flag (see AddressSpaceTests); every other bound is identical.
        self.assertEqual(codex.limits.cpu_seconds, claude.limits.cpu_seconds)
        self.assertEqual(codex.limits.open_files, claude.limits.open_files)
        self.assertEqual(codex.limits.processes, claude.limits.processes)

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


class PromptArgvBoundaryTests(unittest.TestCase):
    """A prompt is the user's words, never an option.

    `provider_argv` places it as a bare positional, so a prompt starting with a
    dash is read as a flag: `--help` printed the help and ran no turn at all,
    and `-c notify=[...]` would have been taken as a config override. Behind
    `--` both are just the prompt. The text is MOVED, never parsed, escaped or
    interpolated.
    """

    def normalize(self, tail, provider="codex"):
        return launch.externally_sandboxed_argv(
            provider, [f"/usr/local/bin/{provider}", *tail]
        )

    def canonical(self, prompt, extra=()):
        return self.normalize([
            "exec", "--json", prompt,
            "--skip-git-repo-check",
            "-c", 'service_tier="default"',
            "-c", "features.fast_mode=false",
            *extra,
        ])

    def test_the_prompt_ends_up_last_and_behind_a_separator(self):
        argv = self.canonical("Reply exactly OK")
        self.assertEqual(argv[-2:], ["--", "Reply exactly OK"])
        self.assertEqual(argv[:4], [
            "/usr/local/bin/codex", "exec",
            launch._EXTERNALLY_SANDBOXED, "--json",
        ])
        # Every option keeps its order and its pairing.
        self.assertEqual(argv[4:-2], [
            "--skip-git-repo-check",
            "-c", 'service_tier="default"',
            "-c", "features.fast_mode=false",
        ])

    def test_a_prompt_that_looks_like_a_flag_stays_the_prompt(self):
        for prompt in (
            "--help",
            "-c notify=[\"/bin/sh\"]",
            "-m gpt-6-astra",
            "--skip-git-repo-check",
            "-",
            "--",
            # The bypass itself: a user may legitimately type it.
            launch._EXTERNALLY_SANDBOXED,
        ):
            with self.subTest(prompt=prompt):
                argv = self.canonical(prompt)
                # Verbatim and last: nothing quoted, escaped or rewritten. A
                # prompt may legitimately equal a real option, so position is
                # what is asserted, not how many times the text appears.
                self.assertEqual(argv[-2:], ["--", prompt])
                self.assertIs(argv[-1], prompt)

    def test_a_prompt_equal_to_the_bypass_does_not_suppress_the_real_flag(self):
        """The trap in a naive `if flag in argv` check.

        A prompt is not an option already present. Suppressing the bypass
        because the user typed its name would silently restore the nested
        sandbox — and fail every command again.
        """
        argv = self.canonical(launch._EXTERNALLY_SANDBOXED)
        self.assertEqual(argv[2], launch._EXTERNALLY_SANDBOXED)  # the real flag
        self.assertEqual(argv[-1], launch._EXTERNALLY_SANDBOXED)  # the prompt
        self.assertEqual(argv.count(launch._EXTERNALLY_SANDBOXED), 2)
        self.assertEqual(argv[-2], "--")

    def test_native_image_pairs_survive_and_stay_before_the_separator(self):
        argv = self.canonical(
            "describe these",
            extra=["--image", "/w/a b.png", "--image", "/w/--odd.png"],
        )
        self.assertEqual(argv[-2:], ["--", "describe these"])
        at = argv.index("--image")
        self.assertEqual(argv[at:at + 4],
                         ["--image", "/w/a b.png", "--image", "/w/--odd.png"])

    def test_it_is_idempotent_and_leaves_an_unknown_shape_alone(self):
        once = self.canonical("hi")
        self.assertEqual(launch.externally_sandboxed_argv("codex", once), once)
        # Not the canonical shape: handed back untouched rather than reordered.
        for tail in (["exec", "hi"], ["exec"], ["app-server"], ["login", "--json", "x"]):
            with self.subTest(tail=tail):
                argv = self.normalize(tail)
                self.assertEqual(argv, [f"/usr/local/bin/codex", *tail])
        # Claude is a different CLI and is never rewritten.
        claude = self.normalize(["exec", "--json", "--help"], provider="claude")
        self.assertEqual(claude, ["/usr/local/bin/claude", "exec", "--json", "--help"])


class AddressSpaceTests(unittest.TestCase):
    """Codex needs room for V8's reservation; nothing else changes.

    Measured on the pinned binary: the code-mode host's V8 sandbox cage peaks at
    ~1.35 TiB of RESERVED address space. At 2 GiB it aborted with "Failed to
    reserve the virtual address space for the V8 sandbox" and at 8 GiB it failed
    the same way. Physical memory is bounded by the container's cgroup, which
    RLIMIT_AS was never doing: the cage is reserved and never committed.
    """

    def test_codex_clears_the_measured_v8_reservation_and_claude_is_unchanged(self):
        measured_cage_bytes = 1_452_350_852 * 1024
        codex = launch.address_space_bytes("codex", None)
        self.assertGreater(codex, measured_cage_bytes)
        self.assertEqual(codex, 2 * 1024**4)
        # Finite, not unlimited: a ceiling is still a ceiling.
        self.assertIsNotNone(codex)
        self.assertEqual(launch.address_space_bytes("claude", None), 2 * 1024**3)

    def test_an_explicit_operator_value_wins_for_every_provider(self):
        for provider in ("codex", "claude"):
            with self.subTest(provider=provider):
                self.assertEqual(launch.address_space_bytes(provider, 4096), 4096)

    def test_the_spec_carries_the_per_provider_default(self):
        def spec_for(provider, extra=()):
            return launch.build_spec(launch._parse([
                "--tenant", "org:3/user:7", "--slot", "a",
                "--workdir", "/var/lib/neev-native/agents/run-abc",
                "--uid", "61000", "--gid", "61000",
                "--provider", provider, "--credential-handle", "h",
                *extra,
                "--", f"/usr/local/bin/{provider}", "exec", "--json", "hi",
            ]))
        self.assertEqual(spec_for("codex").limits.address_space_bytes, 2 * 1024**4)
        self.assertEqual(spec_for("claude").limits.address_space_bytes, 2 * 1024**3)
        self.assertEqual(
            spec_for("codex", ["--address-space-bytes", "12345"])
            .limits.address_space_bytes,
            12345,
        )


class AppServerTransportTests(unittest.TestCase):
    """The `--codex-stream app-server` rewrite.

    `codex exec --json` emits nothing until an item is finished, so a reply
    lands whole after the turn is over. The rewrite runs the adapter as the
    sandboxed provider instead; the adapter drives `codex app-server` over its
    OWN child pipes, inside the boundary. Nothing about the launcher's stdin
    changes — that is the invariant these tests exist to hold.
    """

    def spec_for(self, provider, tail, stream="app-server"):
        parsed = launch._parse([
            "--tenant", "org:3/user:7",
            "--slot", "a",
            "--workdir", "/var/lib/neev-native/agents/run-x",
            "--uid", "61000", "--gid", "61000",
            "--provider", provider,
            "--credential-handle", "handle-1",
            "--codex-stream", stream,
            "--", f"/usr/local/bin/{provider}", *tail,
        ])
        return parsed, launch.build_spec(parsed)

    def test_exec_is_the_default_and_leaves_the_launch_as_it_was(self):
        parsed = launch._parse([
            "--tenant", "org:3/user:7", "--slot", "a",
            "--workdir", "/var/lib/neev-native/agents/run-x",
            "--uid", "61000", "--gid", "61000",
            "--provider", "codex", "--credential-handle", "handle-1",
            "--", "/usr/local/bin/codex", "exec", "--json", "hi",
        ])
        self.assertEqual(parsed.codex_stream, "exec")
        spec = launch.build_spec(parsed)
        self.assertEqual(spec.provider_argv[0], "/usr/local/bin/codex")
        self.assertIn(launch._EXTERNALLY_SANDBOXED, spec.provider_argv)

    def test_a_turn_runs_the_adapter_with_every_selection_carried(self):
        _, spec = self.spec_for("codex", [
            "exec", "--json", "hi",
            "--skip-git-repo-check",
            "-c", 'service_tier="fast"',
            "-c", "features.fast_mode=true",
            "-m", "gpt-5.1-codex",
            "--image", "/w/run-x/a b.png",
        ])
        self.assertEqual(spec.provider_argv, (
            sandbox.PYTHON, "-I", launch.ADAPTER,
            "--codex", "/usr/local/bin/codex",
            # Effort and tier have parameters of their own in the pinned
            # protocol; fast mode has none and stays a config override.
            "--service-tier", "fast",
            "--config", "features.fast_mode=true",
            "--model", "gpt-5.1-codex",
            "--image", "/w/run-x/a b.png",
            "--", "hi",
        ))
        # Codex's own sandbox bypass belongs to `exec`; the adapter says the
        # same thing as thread parameters.
        self.assertNotIn(launch._EXTERNALLY_SANDBOXED, spec.provider_argv)

    def test_the_prompt_is_passed_through_literally(self):
        for prompt in ("--help", "-c notify=[]", "--", "  spaced  "):
            with self.subTest(prompt=prompt):
                _, spec = self.spec_for("codex", ["exec", "--json", prompt])
                self.assertEqual(spec.provider_argv[-2:], ("--", prompt))

    def test_the_reasoning_effort_becomes_a_protocol_parameter(self):
        _, spec = self.spec_for("codex", [
            "exec", "--json", "hi", "-c", 'model_reasoning_effort="high"',
        ])
        self.assertIn("--effort", spec.provider_argv)
        self.assertEqual(
            spec.provider_argv[spec.provider_argv.index("--effort") + 1], "high"
        )

    def test_an_unmapped_option_is_refused_not_silently_dropped(self):
        # An argument this rewrite does not know is a selection the reader made.
        # Running the turn without it would look like it worked.
        for tail in (
            ["exec", "--json", "hi", "--unknown-flag"],
            ["exec", "--json", "hi", "-m"],          # a flag with no value
            ["exec", "hi"],                          # not the canonical shape
            ["login"],
            # An override with no protocol parameter and no place in the table.
            ["exec", "--json", "hi", "-c", 'model_provider="someone_else"'],
            ["exec", "--json", "hi", "-c", "features.fast_mode=maybe"],
        ):
            with self.subTest(tail=tail):
                with self.assertRaises(sandbox.SandboxError):
                    self.spec_for("codex", tail)

    def test_the_metadata_probe_and_claude_are_untouched(self):
        parsed, spec = self.spec_for("codex", ["app-server"])
        self.assertEqual(spec.provider_argv, ("/usr/local/bin/codex", "app-server"))
        self.assertTrue(launch.is_metadata_probe(parsed.provider, spec))
        _, claude = self.spec_for("claude", ["-p", "hi", "--output-format", "stream-json"])
        self.assertEqual(claude.provider_argv[0], "/usr/local/bin/claude")

    def test_a_streaming_turn_is_never_a_metadata_probe(self):
        # The probe is the ONE shape with a writable stdin. A turn that now runs
        # an app-server BEHIND the adapter must not become one: the prompt still
        # arrives as argv and nothing is fed in from outside the sandbox.
        parsed, spec = self.spec_for("codex", ["exec", "--json", "hi"])
        self.assertFalse(launch.is_metadata_probe(parsed.provider, spec))
        self.assertNotIn("app-server", spec.provider_argv)


if __name__ == "__main__":
    unittest.main()
