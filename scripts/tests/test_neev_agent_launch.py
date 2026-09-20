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
            try:
                rc = launch.main(argv, volumes=RaisingVolumes())
            finally:
                if saved is None:
                    os.environ.pop("NEEV_AGENT_WORKSPACE_ROOT", None)
                else:
                    os.environ["NEEV_AGENT_WORKSPACE_ROOT"] = saved
        # Reached credential delivery (parser + spec + workdir all valid) and
        # refused BEFORE any sandbox launch.
        self.assertEqual(rc, 5)


if __name__ == "__main__":
    unittest.main()
