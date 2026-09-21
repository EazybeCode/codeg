"""Portable tests for the production agent launch entrypoint's spec construction.
Actually spawning the sandbox is Linux-only (`neev_agent_sandbox_acceptance.py`);
these prove the CLI maps to a correct, validated SandboxSpec + egress policy."""

import os
import sys
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

        def fake_launch(spec, env=None, bind_source=None):
            captured["env"] = env
            captured["bind_source"] = bind_source
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
