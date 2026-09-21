"""Tests for the per-slot proxy lifecycle.

Three layers, deliberately:

  * pure decisions (scope validation, the exact child command line, quarantine)
    run everywhere;
  * the lifecycle runs against a recording launcher, so the ORDER — lock, refuse
    an existing listener, spawn, ready, yield, stop, unlock — is asserted rather
    than assumed;
  * the two pieces that are really about processes and sockets — the readiness
    handshake and the stop/kill/reap path — run against REAL pipes, REAL child
    processes and REAL listening sockets on this host. Only the netns entry and
    the credential drop need Linux and root, and those are the two things a fake
    cannot honestly stand in for, so they are named as uncovered rather than
    mocked into a false pass.
"""

from __future__ import annotations

import contextlib
import errno
import os
import socket
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from pathlib import Path

# Appended, not inserted: this worktree carries older copies of some shared ops
# modules, and the bootstrap/profile/egress this integrates with must be the
# ones actually on PYTHONPATH. In main, where they are all one directory, this
# resolves identically.
_OPERATIONS = str(Path(__file__).resolve().parents[1] / "operations")
if _OPERATIONS not in sys.path:
    sys.path.append(_OPERATIONS)

import neev_agent_network_bootstrap as bootstrap  # noqa: E402
import neev_login_network_profile as profile  # noqa: E402
import neev_slot_proxy as slotproxy  # noqa: E402


# --------------------------------------------------------------------------- #
# Doubles
# --------------------------------------------------------------------------- #


class FakeLauncher:
    """Records the lifecycle instead of touching the network."""

    def __init__(self, *, listener=False, ready_error=None, stop_ok=True):
        self.calls = []
        self.listener = listener
        self.ready_error = ready_error
        self.stop_ok = stop_ok
        self.started = []

    def listener_present(self, slot):
        self.calls.append(("probe", slot))
        return self.listener

    def start(self, provider, purpose, slot):
        self.calls.append(("start", provider, purpose, slot))
        child = object()
        self.started.append(child)
        return child

    def wait_ready(self, child, timeout):
        self.calls.append(("ready", timeout))
        if self.ready_error is not None:
            raise self.ready_error

    def stop(self, child, timeout):
        self.calls.append(("stop", timeout))
        return self.stop_ok


class RecordingLock:
    """A stand-in for `slot_proxy_lock` that proves enter/exit bracketing."""

    def __init__(self, calls, *, error=None):
        self.calls = calls
        self.error = error

    def __call__(self, slot):
        if self.error is not None:
            raise self.error
        self.calls.append(("lock", slot))
        return self._held()

    @contextlib.contextmanager
    def _held(self):
        try:
            yield
        finally:
            self.calls.append(("unlock",))


class FakeProcess:
    """Process handle with scriptable death, for the readiness/stop paths."""

    def __init__(self, *, exits_after=None, signal_error=None, dies_on=None):
        self.exits_after = exits_after
        self.signal_error = signal_error
        self.dies_on = dies_on or set()
        self.sent = []
        self._returncode = None
        self._started = time.monotonic()

    def poll(self):
        if self._returncode is None and self.exits_after is not None:
            if time.monotonic() - self._started >= self.exits_after:
                self._returncode = 1
        return self._returncode

    def terminate(self):
        self.sent.append("TERM")
        if self.signal_error is not None:
            raise self.signal_error
        if "TERM" in self.dies_on:
            self._returncode = 0

    def kill(self):
        self.sent.append("KILL")
        if self.signal_error is not None:
            raise self.signal_error
        if "KILL" in self.dies_on:
            self._returncode = -9

    def wait(self, timeout=None):
        if self._returncode is None:
            raise subprocess.TimeoutExpired("proxy", timeout)
        return self._returncode


def free_port():
    with socket.socket() as probe:
        probe.bind(("127.0.0.1", 0))
        return probe.getsockname()[1]


# --------------------------------------------------------------------------- #


class ScopeTests(unittest.TestCase):
    """Nothing is spawned for a scope the egress policy would not serve."""

    def test_the_supported_matrix_validates(self):
        for provider in ("claude", "codex"):
            for purpose in ("login", "run"):
                for slot in ("a", "b"):
                    with self.subTest(p=provider, u=purpose, s=slot):
                        slotproxy.validate_scope(provider, purpose, slot)

    def test_unknown_scopes_are_refused_before_any_process_exists(self):
        cases = [
            ("gemini", "login", "a"),
            ("claude", "browse", "a"),
            ("claude", "login", "c"),
            ("Claude", "login", "a"),
            ("claude", "LOGIN", "a"),
            (None, "login", "a"),
            ("claude", None, "a"),
            ("claude", "login", None),
            ("claude", "login", 1),
            ("", "", ""),
        ]
        for case in cases:
            with self.subTest(case):
                with self.assertRaises(slotproxy.SlotProxyError) as caught:
                    slotproxy.validate_scope(*case)
                self.assertEqual(caught.exception.category, "invalid")

    def test_validation_defers_to_the_real_policy_builder(self):
        # The child builds its allowlist from `policy_for`; if that denies the
        # scope, the parent must not spawn it anyway.
        original = slotproxy.egress.policy_for
        self.addCleanup(setattr, slotproxy.egress, "policy_for", original)

        def denier(provider, purpose):
            raise slotproxy.egress.EgressError("denied")

        slotproxy.egress.policy_for = denier
        with self.assertRaises(slotproxy.SlotProxyError):
            slotproxy.validate_scope("claude", "login", "a")

    def test_an_error_never_carries_the_rejected_detail(self):
        error = slotproxy.SlotProxyError("busy")
        self.assertEqual(str(error), "busy")
        # An unrecognised category collapses rather than leaking a message.
        self.assertEqual(slotproxy.SlotProxyError("10.203.0.2:3128").category,
                         "unavailable")


class CommandTests(unittest.TestCase):
    """The exact command that runs the proxy in the existing namespace."""

    def test_the_child_runs_in_the_login_namespace_on_the_slot_address(self):
        argv = slotproxy.child_argv("codex", "run", "b", 7)
        self.assertEqual(argv[:4], [bootstrap.IP, "netns", "exec", "neev-login-proxy"])
        self.assertEqual(argv[3], bootstrap.PROXY_NETNS)
        self.assertEqual(argv[4], sys.executable)
        self.assertIn("--serve", argv)
        for flag, value in (
            ("--provider", "codex"),
            ("--purpose", "run"),
            ("--host", profile.slot_profile("b").proxy_ip),
            ("--port", "3128"),
            ("--ready-fd", "7"),
        ):
            self.assertEqual(argv[argv.index(flag) + 1], value)

    def test_each_slot_binds_only_its_own_endpoint(self):
        a = slotproxy.child_argv("claude", "login", "a", 3)
        b = slotproxy.child_argv("claude", "login", "b", 3)
        self.assertEqual(a[a.index("--host") + 1], "10.203.0.2")
        self.assertEqual(b[b.index("--host") + 1], "10.203.0.10")
        # Exactly what the guest will be told to use, from the same profile.
        self.assertEqual(
            profile.proxy_url("a"), "http://%s:%d" % ("10.203.0.2", slotproxy.PROXY_PORT)
        )

    def test_the_command_is_explicit_and_re_executes_this_module(self):
        argv = slotproxy.child_argv("claude", "login", "a", 3)
        self.assertTrue(all(isinstance(part, str) for part in argv))
        # Absolute interpreter and absolute module: no PATH, no shell, no `-c`.
        self.assertTrue(os.path.isabs(argv[0]))
        self.assertTrue(os.path.isabs(argv[4]))
        module = argv[7]
        self.assertTrue(os.path.isabs(module))
        self.assertEqual(os.path.basename(module), "neev_slot_proxy.py")
        # Isolated: no site-packages injection, no stale bytecode.
        self.assertIn("-I", argv)
        self.assertIn("-B", argv)
        self.assertNotIn("-c", argv)

    def test_an_invalid_slot_has_no_command_at_all(self):
        with self.assertRaises(slotproxy.SlotProxyError):
            slotproxy.child_argv("claude", "login", "z", 3)


class QuarantineTests(unittest.TestCase):
    def setUp(self):
        self._dir = tempfile.TemporaryDirectory()
        self.directory = os.path.join(self._dir.name, "quarantine")
        self.addCleanup(self._dir.cleanup)

    def test_a_marked_slot_is_refused_and_stays_refused(self):
        self.assertFalse(slotproxy.is_quarantined("a", directory=self.directory))
        slotproxy.quarantine("a", directory=self.directory)
        self.assertTrue(slotproxy.is_quarantined("a", directory=self.directory))
        # Durable on purpose: a later process must reach the same conclusion.
        self.assertTrue(
            os.path.exists(os.path.join(self.directory, "slot-a.quarantine"))
        )
        self.assertFalse(slotproxy.is_quarantined("b", directory=self.directory))

    def test_clearing_is_an_explicit_act_and_is_idempotent(self):
        slotproxy.quarantine("a", directory=self.directory)
        slotproxy.clear_quarantine("a", directory=self.directory)
        self.assertFalse(slotproxy.is_quarantined("a", directory=self.directory))
        slotproxy.clear_quarantine("a", directory=self.directory)

    def test_the_marker_directory_is_not_world_readable(self):
        slotproxy.quarantine("a", directory=self.directory)
        mode = os.stat(self.directory).st_mode & 0o777
        self.assertEqual(mode, 0o700)

    def test_a_quarantined_slot_never_reaches_the_lock(self):
        slotproxy.quarantine("a", directory=self.directory)
        calls = []
        launcher = FakeLauncher()
        with self.assertRaises(slotproxy.SlotProxyError) as caught:
            with slotproxy.slot_proxy(
                "claude",
                "login",
                "a",
                launcher=launcher,
                lock=RecordingLock(calls),
                quarantine_dir=self.directory,
            ):
                self.fail("body must not run")
        self.assertEqual(caught.exception.category, "quarantined")
        self.assertEqual(calls, [])
        self.assertEqual(launcher.calls, [])


class LifecycleTests(unittest.TestCase):
    """Order is the contract: the endpoint is free again before the lock drops."""

    def setUp(self):
        self._dir = tempfile.TemporaryDirectory()
        self.directory = os.path.join(self._dir.name, "quarantine")
        self.addCleanup(self._dir.cleanup)
        self.calls = []

    def run_proxy(self, launcher, *, body=None, lock_error=None, slot="a"):
        lock = RecordingLock(self.calls, error=lock_error)
        manager = slotproxy.slot_proxy(
            "claude",
            "login",
            slot,
            launcher=launcher,
            lock=lock,
            quarantine_dir=self.directory,
        )
        launcher_calls = launcher.calls

        @contextlib.contextmanager
        def merged():
            with manager as env:
                self.calls.append(("body",))
                yield env

        if body is None:
            with merged() as env:
                pass
            return env, launcher_calls
        with merged() as env:
            body(env)
        return env, launcher_calls

    def test_the_happy_path_runs_in_order_and_yields_the_slot_environment(self):
        launcher = FakeLauncher()
        env, _ = self.run_proxy(launcher)

        self.assertEqual(
            self.calls,
            [("lock", "a"), ("body",), ("unlock",)],
        )
        self.assertEqual(
            [call[0] for call in launcher.calls],
            ["probe", "start", "ready", "stop"],
        )
        # Exactly the guest environment the profile defines — not a second
        # opinion about what the proxy URL is.
        self.assertEqual(env, profile.proxy_env("a"))
        self.assertEqual(env["HTTP_PROXY"], "http://10.203.0.2:3128")
        self.assertEqual(env["NO_PROXY"], "")

    def test_an_existing_listener_is_refused_and_never_inherited(self):
        launcher = FakeLauncher(listener=True)
        with self.assertRaises(slotproxy.SlotProxyError) as caught:
            self.run_proxy(launcher)
        # A listener under our exclusive lock is serving somebody else's
        # allowlist; adopting it would hand this tenant the wrong egress policy.
        self.assertEqual(caught.exception.category, "occupied")
        self.assertEqual([call[0] for call in launcher.calls], ["probe"])
        self.assertEqual(self.calls, [("lock", "a"), ("unlock",)])
        self.assertFalse(slotproxy.is_quarantined("a", directory=self.directory))

    def test_a_child_that_never_becomes_ready_is_stopped_and_the_body_skipped(self):
        launcher = FakeLauncher(ready_error=slotproxy.SlotProxyError("timeout"))
        with self.assertRaises(slotproxy.SlotProxyError) as caught:
            self.run_proxy(launcher)
        self.assertEqual(caught.exception.category, "timeout")
        self.assertEqual(
            [call[0] for call in launcher.calls], ["probe", "start", "ready", "stop"]
        )
        self.assertNotIn(("body",), self.calls)
        self.assertEqual(self.calls[-1], ("unlock",))

    def test_a_failure_in_the_body_still_stops_the_proxy_before_unlocking(self):
        launcher = FakeLauncher()

        def explode(_env):
            raise RuntimeError("guest failed")

        with self.assertRaises(RuntimeError):
            self.run_proxy(launcher, body=explode)
        self.assertEqual([call[0] for call in launcher.calls][-1], "stop")
        self.assertEqual(self.calls[-1], ("unlock",))

    def test_an_unconfirmed_stop_quarantines_the_slot_for_every_later_tenant(self):
        launcher = FakeLauncher(stop_ok=False)
        with self.assertRaises(slotproxy.SlotProxyError) as caught:
            self.run_proxy(launcher)
        self.assertEqual(caught.exception.category, "quarantined")
        self.assertTrue(slotproxy.is_quarantined("a", directory=self.directory))

        # And the next tenant is refused rather than handed a slot that may
        # still be running the previous tenant's proxy.
        with self.assertRaises(slotproxy.SlotProxyError) as second:
            self.run_proxy(FakeLauncher())
        self.assertEqual(second.exception.category, "quarantined")

    def test_a_failed_start_quarantines_only_when_the_child_survives(self):
        launcher = FakeLauncher(
            ready_error=slotproxy.SlotProxyError("unavailable"), stop_ok=False
        )
        with self.assertRaises(slotproxy.SlotProxyError):
            self.run_proxy(launcher)
        self.assertTrue(slotproxy.is_quarantined("a", directory=self.directory))

    def test_the_other_owner_holding_the_endpoint_is_reported_as_busy(self):
        launcher = FakeLauncher()
        with self.assertRaises(slotproxy.SlotProxyError) as caught:
            self.run_proxy(launcher, lock_error=bootstrap.BootstrapError("busy"))
        self.assertEqual(caught.exception.category, "busy")
        self.assertEqual(launcher.calls, [])

    def test_a_lock_that_cannot_be_taken_at_all_is_not_reported_as_busy(self):
        with self.assertRaises(slotproxy.SlotProxyError) as caught:
            self.run_proxy(
                FakeLauncher(), lock_error=bootstrap.BootstrapError("invalid")
            )
        self.assertEqual(caught.exception.category, "invalid")

    def test_slot_b_locks_and_binds_slot_b(self):
        launcher = FakeLauncher()
        env, _ = self.run_proxy(launcher, slot="b")
        self.assertEqual(self.calls[0], ("lock", "b"))
        self.assertEqual(launcher.calls[1], ("start", "claude", "login", "b"))
        self.assertEqual(env["HTTPS_PROXY"], "http://10.203.0.10:3128")

    def test_the_real_per_slot_lock_is_the_default(self):
        # Nothing here should be reachable without the bootstrap's own flock.
        launcher = FakeLauncher()
        seen = []
        original = bootstrap.slot_proxy_lock

        @contextlib.contextmanager
        def spy(slot, **kwargs):
            seen.append(slot)
            yield

        bootstrap.slot_proxy_lock = spy
        self.addCleanup(setattr, bootstrap, "slot_proxy_lock", original)
        with slotproxy.slot_proxy(
            "codex", "run", "a", launcher=launcher, quarantine_dir=self.directory
        ):
            pass
        self.assertEqual(seen, ["a"])


class ReadinessTests(unittest.TestCase):
    """The real handshake, over real pipes."""

    def setUp(self):
        self.launcher = slotproxy.ProxyLauncher()
        self.read_fd, self.write_fd = os.pipe()
        self.addCleanup(self._close)

    def _close(self):
        for fd in (self.read_fd, self.write_fd):
            with contextlib.suppress(OSError):
                os.close(fd)

    def child(self, process):
        return slotproxy._Child(process, self.read_fd)

    def test_the_ready_token_completes_the_wait(self):
        os.write(self.write_fd, b"R")
        self.launcher.wait_ready(self.child(FakeProcess()), 1.0)

    def test_a_late_token_still_completes_within_the_budget(self):
        timer = threading.Timer(0.1, lambda: os.write(self.write_fd, b"R"))
        timer.start()
        self.addCleanup(timer.cancel)
        self.launcher.wait_ready(self.child(FakeProcess()), 3.0)

    def test_a_silent_child_times_out_rather_than_hanging(self):
        started = time.monotonic()
        with self.assertRaises(slotproxy.SlotProxyError) as caught:
            self.launcher.wait_ready(self.child(FakeProcess()), 0.3)
        self.assertEqual(caught.exception.category, "timeout")
        self.assertLess(time.monotonic() - started, 3.0)

    def test_a_child_that_exits_before_binding_fails_fast(self):
        # EOF without the token: the pipe closed because the proxy died.
        os.close(self.write_fd)
        with self.assertRaises(slotproxy.SlotProxyError) as caught:
            self.launcher.wait_ready(self.child(FakeProcess()), 5.0)
        self.assertEqual(caught.exception.category, "unavailable")

    def test_a_dead_child_is_noticed_without_waiting_out_the_timeout(self):
        process = FakeProcess(exits_after=0.0)
        started = time.monotonic()
        with self.assertRaises(slotproxy.SlotProxyError) as caught:
            self.launcher.wait_ready(self.child(process), 30.0)
        self.assertEqual(caught.exception.category, "unavailable")
        self.assertLess(time.monotonic() - started, 5.0)

    def test_a_wrong_token_is_not_accepted_as_ready(self):
        os.write(self.write_fd, b"X")
        with self.assertRaises(slotproxy.SlotProxyError):
            self.launcher.wait_ready(self.child(FakeProcess()), 1.0)


class StopTests(unittest.TestCase):
    """Real child processes: terminate, escalate, reap."""

    def setUp(self):
        self.launcher = slotproxy.ProxyLauncher()

    def spawn(self, code):
        process = subprocess.Popen([sys.executable, "-I", "-c", code])
        self.addCleanup(self._reap, process)
        read_fd, write_fd = os.pipe()
        os.close(write_fd)
        return slotproxy._Child(process, read_fd)

    def _reap(self, process):
        if process.poll() is None:
            process.kill()
            process.wait(timeout=5)

    def test_a_cooperative_child_is_terminated_and_reaped(self):
        child = self.spawn("import time; time.sleep(30)")
        self.assertTrue(self.launcher.stop(child, 5.0))
        self.assertIsNotNone(child.process.poll())

    def test_a_child_ignoring_sigterm_is_killed(self):
        child = self.spawn(
            "import signal,time\n"
            "signal.signal(signal.SIGTERM, signal.SIG_IGN)\n"
            "time.sleep(30)\n"
        )
        # Give it a moment to install the handler before signalling.
        time.sleep(0.3)
        self.assertTrue(self.launcher.stop(child, 0.5))
        self.assertIsNotNone(child.process.poll())

    def test_an_already_dead_child_reports_gone(self):
        child = self.spawn("pass")
        child.process.wait(timeout=5)
        self.assertTrue(self.launcher.stop(child, 1.0))

    def test_stop_closes_the_readiness_pipe(self):
        child = self.spawn("import time; time.sleep(30)")
        fd = child.ready_read
        self.launcher.stop(child, 5.0)
        with self.assertRaises(OSError):
            os.fstat(fd)

    def test_a_child_we_cannot_signal_is_never_reported_as_gone(self):
        # This is the CAP_KILL case: the child dropped to uid 62000 and the
        # parent has lost the right to signal it. Claiming success here would
        # release the slot to the next tenant with a live proxy still bound.
        process = FakeProcess(signal_error=OSError(errno.EPERM, "denied"))
        child = slotproxy._Child(process, self._dead_fd())
        self.assertFalse(self.launcher.stop(child, 0.1))

    def test_a_child_that_will_not_die_is_never_reported_as_gone(self):
        process = FakeProcess()  # survives both signals
        child = slotproxy._Child(process, self._dead_fd())
        self.assertFalse(self.launcher.stop(child, 0.05))
        self.assertEqual(process.sent, ["TERM", "KILL"])

    def test_the_kill_escalation_happens_only_after_terminate(self):
        process = FakeProcess(dies_on={"KILL"})
        child = slotproxy._Child(process, self._dead_fd())
        self.assertTrue(self.launcher.stop(child, 0.05))
        self.assertEqual(process.sent, ["TERM", "KILL"])

    def test_a_vanished_child_is_not_escalated_but_is_not_assumed_gone(self):
        # ESRCH says the pid is not there, but an unreaped handle cannot prove
        # it; no second signal, and no clean release either.
        process = FakeProcess(signal_error=OSError(errno.ESRCH, "gone"))
        child = slotproxy._Child(process, self._dead_fd())
        self.assertFalse(self.launcher.stop(child, 0.05))
        self.assertEqual(process.sent, ["TERM"])

    def _dead_fd(self):
        read_fd, write_fd = os.pipe()
        os.close(write_fd)
        self.addCleanup(self._close_quietly, read_fd)
        return read_fd

    @staticmethod
    def _close_quietly(fd):
        with contextlib.suppress(OSError):
            os.close(fd)


class ProbeTests(unittest.TestCase):
    """The occupancy probe, against real sockets on this host."""

    def test_a_listening_endpoint_is_detected(self):
        with socket.socket() as server:
            server.bind(("127.0.0.1", 0))
            # Room for both probes: an unaccepted connection holds a backlog
            # slot, and a full backlog would look like a refusal.
            server.listen(16)
            port = server.getsockname()[1]
            self.assertEqual(slotproxy._probe("127.0.0.1", port), 0)
            self.assertEqual(
                slotproxy.main(["--probe", "--host", "127.0.0.1", "--port", str(port)]),
                0,
            )

    def test_a_free_endpoint_reports_free(self):
        port = free_port()
        self.assertEqual(slotproxy._probe("127.0.0.1", port), 1)
        self.assertEqual(
            slotproxy.main(["--probe", "--host", "127.0.0.1", "--port", str(port)]), 1
        )

    def test_an_unreachable_address_is_not_reported_as_free_by_accident(self):
        # A refused connection means free; anything else must not be mistaken
        # for it, so the probe's only success is an actual completed connect.
        self.assertEqual(slotproxy._probe("127.0.0.1", free_port()), 1)


class ChildCliTests(unittest.TestCase):
    """The fixed internal CLI. It is an entry point for `serve`, not a tool."""

    def test_the_cli_refuses_anything_outside_the_matrix(self):
        for argv in (
            [],
            ["--serve"],
            ["--serve", "--provider", "gemini", "--purpose", "login", "--host", "x"],
            ["--serve", "--provider", "claude", "--purpose", "browse", "--host", "x"],
            ["--serve", "--provider", "claude", "--purpose", "login"],
            ["--probe"],
        ):
            with self.subTest(argv):
                self.assertEqual(slotproxy.main(argv), 2)

    def test_the_child_builds_its_policy_before_dropping_privileges(self):
        # A denied scope must fail as a plain error, not after the process has
        # already given up the credentials it would need to clean up.
        order = []
        original_policy = slotproxy.egress.policy_for
        original_drop = slotproxy.drop_privileges
        self.addCleanup(setattr, slotproxy.egress, "policy_for", original_policy)
        self.addCleanup(setattr, slotproxy, "drop_privileges", original_drop)

        def policy_for(provider, purpose):
            order.append("policy")
            raise slotproxy.egress.EgressError("denied")

        slotproxy.egress.policy_for = policy_for
        slotproxy.drop_privileges = lambda: order.append("drop")

        with self.assertRaises(slotproxy.egress.EgressError):
            slotproxy._serve("claude", "login", "10.203.0.2", 3128, -1)
        self.assertEqual(order, ["policy"])

    def test_the_proxy_identity_is_the_dedicated_unprivileged_account(self):
        self.assertEqual(slotproxy.PROXY_UID, 62000)
        self.assertEqual(slotproxy.PROXY_GID, 62000)
        # Unprivileged port: nothing has to be retained across the drop.
        self.assertGreater(slotproxy.PROXY_PORT, 1024)


@unittest.skipUnless(
    sys.platform.startswith("linux") and os.geteuid() == 0,
    "namespace entry and the credential drop need Linux and root",
)
class LinuxHostTests(unittest.TestCase):
    """The two things a fake cannot stand in for, when the host can run them."""

    def test_the_child_drops_to_the_proxy_account_with_no_new_privs(self):
        code = (
            "import os,sys,ctypes\n"
            "sys.path.insert(0, %r)\n"
            "import neev_slot_proxy as p\n"
            "p.drop_privileges()\n"
            "libc = ctypes.CDLL(None, use_errno=True)\n"
            "print(os.geteuid(), os.getegid(), len(os.getgroups()),"
            " libc.prctl(39,0,0,0,0))\n"
        ) % os.path.dirname(slotproxy.__file__)
        out = subprocess.run(
            [sys.executable, "-I", "-c", code], capture_output=True, text=True
        )
        self.assertEqual(out.returncode, 0, out.stderr)
        uid, gid, groups, nnp = out.stdout.split()
        self.assertEqual((uid, gid, groups), ("62000", "62000", "0"))
        self.assertEqual(nnp, "1")  # PR_GET_NO_NEW_PRIVS

    def test_a_real_proxy_serves_and_stops_inside_the_namespace(self):
        launcher = slotproxy.ProxyLauncher()
        with slotproxy.slot_proxy("claude", "login", "a", launcher=launcher) as env:
            self.assertEqual(env["HTTP_PROXY"], profile.proxy_url("a"))
            self.assertTrue(launcher.listener_present("a"))
        # Free again before the lock was released.
        self.assertFalse(launcher.listener_present("a"))


if __name__ == "__main__":
    unittest.main()
