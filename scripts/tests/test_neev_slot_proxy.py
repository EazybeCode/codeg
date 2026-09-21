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
import importlib.util
import json
import os
import shutil
import socket
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from pathlib import Path

# Shared ops modules (bootstrap, profile, egress) come from wherever they
# actually live — this worktree carries stale partial copies, so its directory
# is APPENDED rather than inserted. The module under test, by contrast, is
# loaded by explicit path: main may already carry an older cherry-pick of it,
# and that copy must never be what these tests exercise.
_OPERATIONS = str(Path(__file__).resolve().parents[1] / "operations")
if _OPERATIONS not in sys.path:
    sys.path.append(_OPERATIONS)

import neev_agent_network_bootstrap as bootstrap  # noqa: E402
import neev_login_network_profile as profile  # noqa: E402

_MODULE_PATH = os.path.join(_OPERATIONS, "neev_slot_proxy.py")
_SPEC = importlib.util.spec_from_file_location("neev_slot_proxy", _MODULE_PATH)
slotproxy = importlib.util.module_from_spec(_SPEC)
sys.modules["neev_slot_proxy"] = slotproxy
_SPEC.loader.exec_module(slotproxy)

# Where the shared ops modules actually live, taken from one that is already
# imported rather than guessed.
_SHARED_OPERATIONS = os.path.dirname(os.path.abspath(bootstrap.__file__))


def stage_operations(mode=0o755):
    """A single directory holding the module and everything it imports.

    The child runs under `-I` with no PYTHONPATH, so its whole import closure
    has to sit beside it. That is true of the deployed layout and of main; it is
    not true of this worktree, which carries only some of the shared modules.
    """
    staged = tempfile.mkdtemp()
    for name in os.listdir(_SHARED_OPERATIONS):
        if name.endswith(".py"):
            shutil.copy(os.path.join(_SHARED_OPERATIONS, name), staged)
    # Last, so the module under test is this worktree's and not any older copy
    # already sitting in the shared directory.
    shutil.copy(_MODULE_PATH, staged)
    os.chmod(staged, mode)
    return staged


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



class FakeOps:
    """Records the credential sequence so its ORDER can be asserted off-Linux."""

    def __init__(self, *, bounding=None, fail=None, state=None, euid=0, last=40):
        self.calls = []
        self.bounding = set(range(last + 1)) if bounding is None else set(bounding)
        self.fail = fail or {}
        self.euid = euid
        self.egid = 0
        self.last = last
        if state is None:
            state = {name: 0 for name in slotproxy._CAPABILITY_FIELDS}
            state["NoNewPrivs"] = 1
        self.state = state

    def geteuid(self):
        return self.euid

    def getegid(self):
        return self.egid

    def cap_last_cap(self):
        return self.last

    def prctl(self, option, arg2=0, arg3=0, arg4=0, arg5=0):
        if option == slotproxy.PR_CAPBSET_READ:
            if arg2 > self.last:
                return -1
            return 1 if arg2 in self.bounding else 0
        if option == slotproxy.PR_CAPBSET_DROP:
            self.calls.append(("capbset_drop", arg2))
            if "capbset_drop" in self.fail:
                return self.fail["capbset_drop"]
            self.bounding.discard(arg2)
            return 0
        if option == slotproxy.PR_SET_NO_NEW_PRIVS:
            self.calls.append(("no_new_privs",))
            return self.fail.get("no_new_privs", 0)
        if option == slotproxy.PR_CAP_AMBIENT:
            self.calls.append(("ambient_clear",))
            return self.fail.get("ambient_clear", 0)
        raise AssertionError("unexpected prctl option %r" % option)

    def setgroups(self, groups):
        self.calls.append(("setgroups", tuple(groups)))

    def setresgid(self, gid):
        self.calls.append(("setresgid", gid))
        self.egid = gid

    def setresuid(self, uid):
        self.calls.append(("setresuid", uid))
        self.euid = uid

    def zero_capabilities(self):
        self.calls.append(("capset_zero",))
        return self.fail.get("capset_zero", 0)

    def capability_state(self):
        self.calls.append(("read_state",))
        return dict(self.state)

    def order(self):
        return [call[0] for call in self.calls]



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
    """The occupancy probe, against real sockets on this host.

    Absence has exactly one exit code. Everything else means the question went
    unanswered, and an unanswered question must not read as a free endpoint.
    """

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

    def test_only_a_refused_connection_reports_free(self):
        port = free_port()
        self.assertEqual(slotproxy._probe("127.0.0.1", port), slotproxy.PROBE_FREE)
        self.assertEqual(
            slotproxy.main(["--probe", "--host", "127.0.0.1", "--port", str(port)]), 1
        )

    def test_a_probe_that_could_not_find_out_says_so_instead_of_free(self):
        # A resolver failure is not evidence that nothing is listening.
        self.assertEqual(
            slotproxy._probe("no-such-host.invalid.", 3128),
            slotproxy.PROBE_UNDETERMINED,
        )
        self.assertNotEqual(slotproxy.PROBE_UNDETERMINED, slotproxy.PROBE_FREE)


class ListenerPresentTests(unittest.TestCase):
    """How the parent reads the probe's exit code."""

    class _Result:
        def __init__(self, returncode):
            self.returncode = returncode

    def run_with(self, outcome):
        launcher = slotproxy.ProxyLauncher()
        original = slotproxy.subprocess
        self.addCleanup(setattr, slotproxy, "subprocess", original)

        class _Fake:
            SubprocessError = original.SubprocessError
            TimeoutExpired = original.TimeoutExpired

            @staticmethod
            def run(argv, timeout=None):
                if isinstance(outcome, Exception):
                    raise outcome
                return ListenerPresentTests._Result(outcome)

        slotproxy.subprocess = _Fake
        return launcher.listener_present("a")

    def test_zero_is_occupied_and_one_is_free(self):
        self.assertTrue(self.run_with(0))
        self.assertFalse(self.run_with(1))

    def test_any_other_exit_is_unavailable_never_free(self):
        # 3 = the probe could not determine; 2 = it rejected its own arguments;
        # -9 = it was killed. None of these is evidence of a free endpoint, and
        # treating them as one starts a second proxy over a live one.
        for outcome in (3, 2, -9, -15, 127, 255):
            with self.subTest(outcome):
                with self.assertRaises(slotproxy.SlotProxyError) as caught:
                    self.run_with(outcome)
                self.assertEqual(caught.exception.category, "unavailable")

    def test_a_probe_that_will_not_run_is_unavailable(self):
        for error in (
            OSError("no ip binary"),
            subprocess.TimeoutExpired("probe", 1),
        ):
            with self.subTest(type(error).__name__):
                with self.assertRaises(slotproxy.SlotProxyError):
                    self.run_with(error)


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


class PrivilegeDropTests(unittest.TestCase):
    """The credential drop, asserted by call order and by the kernel's own answer."""

    def test_the_bounding_set_is_emptied_while_root_still_holds_cap_setpcap(self):
        ops = FakeOps(last=40)
        slotproxy.drop_privileges(ops)

        order = ops.order()
        # THE bug this pins: PR_CAPBSET_DROP needs CAP_SETPCAP in the effective
        # set, and setresuid(62000) is exactly what removes it. Every drop must
        # happen before the identity changes, or all of them return EPERM.
        last_drop = max(i for i, name in enumerate(order) if name == "capbset_drop")
        self.assertLess(last_drop, order.index("setresuid"))
        self.assertLess(order.index("no_new_privs"), order.index("capbset_drop"))
        self.assertEqual(ops.bounding, set())

    def test_the_full_sequence_runs_in_the_order_the_kernel_requires(self):
        ops = FakeOps(bounding={0, 8}, last=40)
        slotproxy.drop_privileges(ops)
        self.assertEqual(
            [name for name in ops.order() if name != "capbset_drop"],
            [
                "no_new_privs",
                "setgroups",
                "setresgid",
                "setresuid",
                "ambient_clear",
                "capset_zero",
                "read_state",
            ],
        )
        # CAP_SETPCAP is dropped last of the bounding set: it authorises the rest.
        drops = [call[1] for call in ops.calls if call[0] == "capbset_drop"]
        self.assertEqual(drops, [0, slotproxy.CAP_SETPCAP])
        self.assertIn(("setgroups", ()), ops.calls)
        self.assertIn(("setresgid", 62000), ops.calls)
        self.assertIn(("setresuid", 62000), ops.calls)

    def test_a_refused_bounding_drop_is_not_swallowed(self):
        # EPERM here was previously discarded, leaving a populated bounding set
        # behind a comment claiming it was empty.
        ops = FakeOps(fail={"capbset_drop": -1})
        with self.assertRaises(slotproxy.SlotProxyError):
            slotproxy.drop_privileges(ops)
        self.assertNotIn("setresuid", ops.order())

    def test_every_other_failed_call_also_refuses(self):
        for failure in ("no_new_privs", "ambient_clear", "capset_zero"):
            with self.subTest(failure):
                ops = FakeOps(fail={failure: -1})
                with self.assertRaises(slotproxy.SlotProxyError):
                    slotproxy.drop_privileges(ops)

    def test_capabilities_the_kernel_does_not_know_are_skipped_not_dropped(self):
        ops = FakeOps(bounding={0, 1}, last=3)
        slotproxy.drop_privileges(ops)
        # Only capabilities actually present are dropped; an unknown index
        # returns -1 from CAPBSET_READ and must not be treated as a failure.
        self.assertEqual(
            sorted(call[1] for call in ops.calls if call[0] == "capbset_drop"), [0, 1]
        )

    def test_the_kernel_has_the_last_word_even_when_every_call_succeeded(self):
        for field in slotproxy._CAPABILITY_FIELDS:
            with self.subTest(field):
                state = {name: 0 for name in slotproxy._CAPABILITY_FIELDS}
                state["NoNewPrivs"] = 1
                state[field] = 0x20  # one capability left behind
                ops = FakeOps(state=state)
                with self.assertRaises(slotproxy.SlotProxyError):
                    slotproxy.drop_privileges(ops)

    def test_the_final_check_requires_no_new_privs_to_be_on(self):
        state = {name: 0 for name in slotproxy._CAPABILITY_FIELDS}
        state["NoNewPrivs"] = 0
        with self.assertRaises(slotproxy.SlotProxyError):
            slotproxy.drop_privileges(FakeOps(state=state))

    def test_landing_on_the_wrong_identity_refuses(self):
        # Never reachable in production, but a half-dropped identity must not
        # proceed to bind as though it were the proxy account.
        ops = FakeOps(euid=1000)
        with self.assertRaises(slotproxy.SlotProxyError):
            slotproxy.drop_privileges(ops)


class CapabilityStateTests(unittest.TestCase):
    """Parsing the kernel's answer, which is the only thing that is trusted."""

    def setUp(self):
        self._dir = tempfile.TemporaryDirectory()
        self.addCleanup(self._dir.cleanup)

    def write(self, text):
        path = os.path.join(self._dir.name, "status")
        with open(path, "w", encoding="utf-8") as handle:
            handle.write(text)
        return path

    def test_the_five_masks_and_no_new_privs_are_read_as_numbers(self):
        path = self.write(
            "Name:\tpython3\n"
            "Uid:\t62000\t62000\t62000\n"
            "CapInh:\t0000000000000000\n"
            "CapPrm:\t0000000000000000\n"
            "CapEff:\t0000000000000000\n"
            "CapBnd:\t000001ffffffffff\n"
            "CapAmb:\t0000000000000000\n"
            "NoNewPrivs:\t1\n"
        )
        state = slotproxy.read_capability_state(path)
        self.assertEqual(state["CapBnd"], 0x1FFFFFFFFFF)
        self.assertEqual(state["CapEff"], 0)
        self.assertEqual(state["NoNewPrivs"], 1)
        # A populated bounding set is exactly the failure root found on the host.
        with self.assertRaises(slotproxy.SlotProxyError):
            slotproxy.verify_dropped(state)

    def test_a_fully_dropped_process_verifies(self):
        path = self.write(
            "CapInh:\t0000000000000000\n"
            "CapPrm:\t0000000000000000\n"
            "CapEff:\t0000000000000000\n"
            "CapBnd:\t0000000000000000\n"
            "CapAmb:\t0000000000000000\n"
            "NoNewPrivs:\t1\n"
        )
        slotproxy.verify_dropped(slotproxy.read_capability_state(path))

    def test_an_unreadable_or_unparsable_status_is_refused_not_assumed_empty(self):
        with self.assertRaises(slotproxy.SlotProxyError):
            slotproxy.read_capability_state(os.path.join(self._dir.name, "absent"))
        garbled = self.write("CapBnd:\tnot-a-number\n")
        with self.assertRaises(slotproxy.SlotProxyError):
            slotproxy.read_capability_state(garbled)

    def test_a_missing_mask_is_refused_but_an_absent_ambient_set_is_zero(self):
        # No CapBnd line means we did not read what we think we read.
        with self.assertRaises(slotproxy.SlotProxyError):
            slotproxy.verify_dropped(
                {"CapEff": 0, "CapPrm": 0, "CapInh": 0, "CapAmb": 0, "NoNewPrivs": 1}
            )
        # A kernel with no ambient capabilities at all is genuinely zero.
        slotproxy.verify_dropped(
            {"CapBnd": 0, "CapEff": 0, "CapPrm": 0, "CapInh": 0, "NoNewPrivs": 1}
        )


class IsolatedInterpreterTests(unittest.TestCase):
    """The child really runs under `-I`. These invoke it exactly as it is spawned.

    `-I` drops the script's directory from `sys.path`, which is what made every
    spawned proxy die on `ModuleNotFoundError` before it could bind. The flag is
    kept — it is what keeps a hostile CWD and PYTHONPATH out of a process that
    starts as root — and the one directory it needs is put back explicitly,
    after checking nobody else can write to it.
    """

    @classmethod
    def setUpClass(cls):
        cls.staged = stage_operations()
        cls.module = os.path.join(cls.staged, "neev_slot_proxy.py")

    @classmethod
    def tearDownClass(cls):
        shutil.rmtree(cls.staged, ignore_errors=True)

    def isolated(self, *args, module=None):
        environment = dict(os.environ)
        environment.pop("PYTHONPATH", None)
        # No PYTHONPATH, no CWD on the path, no user site-packages: exactly the
        # child's situation.
        return subprocess.run(
            [sys.executable, "-I", "-B", module or self.module, *args],
            capture_output=True,
            text=True,
            env=environment,
            cwd="/",
        )

    def test_the_module_imports_under_dash_I_with_no_pythonpath(self):
        out = self.isolated("--help")
        self.assertNotIn("ModuleNotFoundError", out.stderr)
        self.assertEqual(out.returncode, 0, out.stderr)
        self.assertIn("--serve", out.stdout)

    def test_the_probe_subcommand_answers_from_a_real_isolated_child(self):
        with socket.socket() as server:
            server.bind(("127.0.0.1", 0))
            server.listen(16)
            port = server.getsockname()[1]
            busy = self.isolated("--probe", "--host", "127.0.0.1", "--port", str(port))
            self.assertNotIn("ModuleNotFoundError", busy.stderr)
            self.assertEqual(busy.returncode, slotproxy.PROBE_LISTENING, busy.stderr)
        free = self.isolated(
            "--probe", "--host", "127.0.0.1", "--port", str(free_port())
        )
        self.assertEqual(free.returncode, slotproxy.PROBE_FREE, free.stderr)

    def test_the_serve_flags_this_module_builds_are_accepted_by_the_child(self):
        # The real argv minus only the `ip netns exec` prefix that needs Linux.
        # A rejected flag would exit 2 before ever binding.
        argv = slotproxy.child_argv("claude", "login", "a", 3)
        self.assertEqual(argv[4], sys.executable)
        flags = argv[8:]
        self.assertIn("--serve", flags)
        out = self.isolated(*[flag for flag in flags], module=self.module)
        # It will fail to bind 10.203.0.2 off Linux, but it must get that far:
        # not an import error and not an argument rejection.
        self.assertNotIn("ModuleNotFoundError", out.stderr)
        self.assertNotEqual(out.returncode, 2, out.stderr)

    def test_the_child_refuses_a_module_directory_others_can_write(self):
        # A writable module directory is a way to make this root process import
        # somebody else's code, which is the whole reason `-I` is there.
        staged = stage_operations(mode=0o777)
        self.addCleanup(shutil.rmtree, staged, True)
        out = self.isolated(
            "--probe",
            "--host",
            "127.0.0.1",
            "--port",
            str(free_port()),
            module=os.path.join(staged, "neev_slot_proxy.py"),
        )
        # Not merely non-zero: it must not collide with PROBE_FREE, or the
        # parent would read a refused startup as a free endpoint.
        self.assertEqual(out.returncode, slotproxy.BOOTSTRAP_EXIT, out.stderr)
        self.assertNotEqual(slotproxy.BOOTSTRAP_EXIT, slotproxy.PROBE_FREE)
        self.assertNotEqual(slotproxy.BOOTSTRAP_EXIT, slotproxy.PROBE_LISTENING)
        self.assertIn("writable by others", out.stderr)


@unittest.skipUnless(
    sys.platform.startswith("linux") and os.geteuid() == 0,
    "namespace entry and the credential drop need Linux and root",
)
class LinuxHostTests(unittest.TestCase):
    """The two things a fake cannot stand in for, when the host can run them."""

    def test_the_child_ends_with_every_capability_set_empty(self):
        code = (
            "import json,os,sys\n"
            "sys.path.insert(0, %r)\n"
            "import neev_slot_proxy as p\n"
            "p.drop_privileges()\n"
            "state = p.read_capability_state()\n"
            "state['uid'] = os.geteuid(); state['gid'] = os.getegid()\n"
            "state['groups'] = len(os.getgroups())\n"
            "print(json.dumps(state))\n"
        ) % os.path.dirname(slotproxy.__file__)
        out = subprocess.run(
            [sys.executable, "-I", "-c", code], capture_output=True, text=True
        )
        self.assertEqual(out.returncode, 0, out.stderr)
        state = json.loads(out.stdout)

        # The identity.
        self.assertEqual(
            (state["uid"], state["gid"], state["groups"]), (62000, 62000, 0)
        )
        # And the capabilities as the KERNEL reports them, not as the calls
        # claimed. CapBnd is the one that silently survived before.
        for field in ("CapBnd", "CapEff", "CapPrm", "CapInh", "CapAmb"):
            with self.subTest(field):
                self.assertEqual(state.get(field, 0), 0)
        self.assertEqual(state["NoNewPrivs"], 1)

    def test_a_real_proxy_serves_and_stops_inside_the_namespace(self):
        launcher = slotproxy.ProxyLauncher()
        with slotproxy.slot_proxy("claude", "login", "a", launcher=launcher) as env:
            self.assertEqual(env["HTTP_PROXY"], profile.proxy_url("a"))
            self.assertTrue(launcher.listener_present("a"))
        # Free again before the lock was released.
        self.assertFalse(launcher.listener_present("a"))


if __name__ == "__main__":
    unittest.main()
