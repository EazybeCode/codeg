#!/usr/bin/env python3
"""Run the per-slot CONNECT proxy for exactly one provider+purpose, then stop it.

`neev_provider_egress.serve()` is an async coroutine with no entry point, and
`neev_agent_network_bootstrap.slot_proxy_lock` only makes the slot endpoint
mutually exclusive. Between them nothing ever *listened*: a guest got
`HTTP_PROXY=http://10.203.0.2:3128` pointing at a closed port. This module is
the missing lifecycle.

`slot_proxy(provider, purpose, slot)` is a context manager that, for its body:

  * validates the scope through `policy_for`, so an unknown provider or purpose
    fails before anything is spawned;
  * takes the exclusive per-slot lock, so two tenants can never both own the
    endpoint;
  * refuses to start if something is ALREADY listening there — a live listener
    under our lock is a listener serving somebody else's allowlist, and
    inheriting it would hand this tenant the wrong egress policy;
  * spawns this same file as a child, `ip netns exec` into the existing
    `neev-login-proxy` namespace, bound to that slot's own proxy IP and 3128;
  * has the CHILD drop to uid/gid 62000 with no-new-privs and an empty
    capability set before it binds — the parent needs root to enter the
    namespace, the proxy never does, and 3128 is unprivileged so nothing is
    retained for the bind;
  * waits for a readiness byte on an inherited pipe, bounded, and only while the
    child is still alive — a dead child fails fast instead of timing out;
  * yields the proxy environment for that slot;
  * stops and reaps the child BEFORE releasing the lock, so the endpoint is free
    the moment the next tenant can take it.

If the child cannot be confirmed gone, the slot is QUARANTINED: a marker is
written and every later `slot_proxy` for that slot refuses. A proxy that might
still be running is a proxy that might still be serving the previous tenant's
allowlist, and reusing the endpoint would silently cross two tenants' policies.
Clearing a quarantine is a deliberate operator act (`clear_quarantine`).

Nothing here widens the network: the namespace, its addresses and its filtering
are the bootstrap's, already in place; this only puts a correctly-scoped
listener inside it for as long as it is needed.
"""

from __future__ import annotations

import argparse
import contextlib
import errno
import os
import signal
import socket
import stat
import subprocess
import sys
import time
from dataclasses import dataclass
from typing import Optional


# Exit code for a refusal during startup, kept clear of the probe codes below.
BOOTSTRAP_EXIT = 4


def _trusted_module_directory() -> str:
    """This file's own directory, once it is proved safe to import from.

    The child runs under `-I`, which is what keeps a hostile CWD, `PYTHONPATH`
    and user site-packages out of a process that starts as root — and which also
    removes the script's directory from `sys.path`, so the imports below would
    otherwise fail outright. Putting exactly one directory back is the narrow
    fix, but only after checking nobody else can write to it: a group- or
    world-writable module directory is a way to have this root process import
    somebody else's code, which is the very thing `-I` is here to prevent.
    """
    directory = os.path.dirname(os.path.abspath(__file__))
    try:
        info = os.stat(directory)
    except OSError:
        _bootstrap_failure("module directory is unreadable")
    if info.st_mode & (stat.S_IWGRP | stat.S_IWOTH):
        _bootstrap_failure("module directory is writable by others")
    if info.st_uid not in (0, os.geteuid()):
        _bootstrap_failure("module directory is owned by another user")
    return directory


def _bootstrap_failure(reason: str):
    """Exit on a distinct code, because 1 already means "nothing is listening".

    The parent reads this process's exit status, and the one code that means a
    free endpoint must mean only that. A startup refusal sharing it would be
    read as permission to start a second proxy.
    """
    sys.stderr.write("neev_slot_proxy: %s\n" % reason)
    raise SystemExit(BOOTSTRAP_EXIT)


if __name__ == "__main__":  # pragma: no cover - child entry only
    _MODULE_DIRECTORY = _trusted_module_directory()
    if _MODULE_DIRECTORY not in sys.path:
        sys.path.insert(0, _MODULE_DIRECTORY)

import neev_agent_network_bootstrap as bootstrap  # noqa: E402
import neev_login_network_profile as profile  # noqa: E402
import neev_provider_egress as egress  # noqa: E402


PROXY_PORT = 3128
# The proxy runs as nobody in particular: its own uid, no groups, no caps.
PROXY_UID = 62000
PROXY_GID = 62000

VALID_PROVIDERS = frozenset(("claude", "codex", "cursor", "gemini", "opencode"))
VALID_PURPOSES = frozenset(("login", "run"))

# Bounded waits. Ready is short because the child only has to bind a socket;
# stop gives a SIGTERM time to unwind before the SIGKILL that root proved needs
# CAP_KILL to reach a uid-dropped child.
READY_TIMEOUT_SECONDS = 10.0
STOP_TIMEOUT_SECONDS = 5.0
PROBE_TIMEOUT_SECONDS = 1.0

# Probe exit codes. Absence has exactly ONE code; everything else is a question
# we could not answer, and an unanswered question is not a free endpoint.
PROBE_LISTENING = 0
PROBE_FREE = 1
PROBE_UNDETERMINED = 3

_QUARANTINE_DIR = "/var/lib/neev-supervisor/proxy-quarantine"


class SlotProxyError(Exception):
    """A fixed category; never carries a host, path or policy detail."""

    _CATEGORIES = frozenset(
        ("invalid", "busy", "occupied", "unavailable", "quarantined", "timeout")
    )

    def __init__(self, category: str):
        if category not in self._CATEGORIES:
            category = "unavailable"
        super().__init__(category)
        self.category = category


def validate_scope(provider: str, purpose: str, slot: str, extra_hosts=()) -> None:
    """Reject anything outside the supported matrix BEFORE spawning.

    `policy_for` is the authority on provider+purpose — it is what the child
    will build its allowlist from — so it is consulted here rather than
    duplicated, and a scope it denies never reaches a subprocess.
    """
    if not _valid_scope_names(provider, purpose):
        raise SlotProxyError("invalid")
    if type(slot) is not str or slot not in ("a", "b"):
        raise SlotProxyError("invalid")
    try:
        egress.policy_for(provider, purpose, extra_hosts=tuple(extra_hosts))
    except Exception:
        raise SlotProxyError("invalid") from None


def _valid_scope_names(provider, purpose) -> bool:
    """A provider+purpose this proxy serves: the provider matrix, or the
    integration-only scope an admin's integration test runs under."""
    if (provider, purpose) == egress.INTEGRATIONS_SCOPE:
        return True
    return (type(provider) is str and provider in VALID_PROVIDERS
            and type(purpose) is str and purpose in VALID_PURPOSES)


def proxy_bind_ip(slot: str) -> str:
    """The address this slot's proxy must listen on, from the fixed profile."""
    try:
        return profile.slot_profile(slot).proxy_ip
    except Exception:
        raise SlotProxyError("invalid") from None


def child_argv(provider: str, purpose: str, slot: str, ready_fd: int, extra_hosts=()) -> list:
    """The exact command that runs the proxy inside the login namespace.

    Fixed and fully explicit: no shell, no PATH lookup for `ip`, and the module
    re-executed by absolute path so the child is this same code.
    """
    return [
        bootstrap.IP,
        "netns",
        "exec",
        bootstrap.PROXY_NETNS,
        sys.executable,
        "-I",
        "-B",
        os.path.abspath(__file__),
        "--serve",
        "--provider",
        provider,
        "--purpose",
        purpose,
        "--host",
        proxy_bind_ip(slot),
        "--port",
        str(PROXY_PORT),
        "--ready-fd",
        str(ready_fd),
    ] + [arg for host in extra_hosts for arg in ("--extra-host", host)]


# --------------------------------------------------------------------------- #
# Quarantine
# --------------------------------------------------------------------------- #


def _quarantine_path(directory: str, slot: str) -> str:
    return os.path.join(directory, "slot-%s.quarantine" % slot)


def is_quarantined(slot: str, *, directory: Optional[str] = None) -> bool:
    return os.path.exists(_quarantine_path(directory or _QUARANTINE_DIR, slot))


def quarantine(slot: str, *, directory: Optional[str] = None) -> None:
    """Mark a slot unusable. Survives this process on purpose."""
    directory = directory or _QUARANTINE_DIR
    try:
        os.makedirs(directory, mode=0o700, exist_ok=True)
        fd = os.open(
            _quarantine_path(directory, slot),
            os.O_WRONLY | os.O_CREAT | os.O_NOFOLLOW | os.O_CLOEXEC,
            0o600,
        )
        os.close(fd)
    except OSError:
        # Even a failed marker must not be reported as a clean release.
        pass


def clear_quarantine(slot: str, *, directory: Optional[str] = None) -> None:
    """Operator action: the slot has been inspected and is known clean."""
    try:
        os.unlink(_quarantine_path(directory or _QUARANTINE_DIR, slot))
    except FileNotFoundError:
        pass
    except OSError:
        raise SlotProxyError("unavailable") from None


# --------------------------------------------------------------------------- #
# Launcher seam
# --------------------------------------------------------------------------- #


@dataclass
class _Child:
    process: object
    ready_read: int


class ProxyLauncher:
    """Spawns and stops the namespaced child. Replaced wholesale in tests."""

    def listener_present(self, slot: str) -> bool:
        """True if something is already bound to this slot's proxy endpoint.

        Probed from inside the namespace, because that is the only place the
        address exists. Only the one defined absence code is read as free: a
        crash, a signal, or any other exit is `unavailable`, because starting a
        second proxy on top of a live one is worse than refusing to start.
        """
        argv = [
            bootstrap.IP,
            "netns",
            "exec",
            bootstrap.PROXY_NETNS,
            sys.executable,
            "-I",
            "-B",
            os.path.abspath(__file__),
            "--probe",
            "--host",
            proxy_bind_ip(slot),
            "--port",
            str(PROXY_PORT),
        ]
        try:
            completed = subprocess.run(argv, timeout=READY_TIMEOUT_SECONDS)
        except (OSError, subprocess.SubprocessError):
            # If we cannot tell, we must not assume the endpoint is free.
            raise SlotProxyError("unavailable") from None
        if completed.returncode == PROBE_LISTENING:
            return True
        if completed.returncode == PROBE_FREE:
            return False
        # Undetermined, killed by a signal, or a probe that failed to start at
        # all: no answer, so no start.
        raise SlotProxyError("unavailable")

    def start(self, provider: str, purpose: str, slot: str, extra_hosts=()):  # pragma: no cover
        read_fd, write_fd = os.pipe()
        os.set_inheritable(write_fd, True)
        argv = child_argv(provider, purpose, slot, write_fd, extra_hosts=tuple(extra_hosts))
        try:
            process = subprocess.Popen(argv, pass_fds=(write_fd,), close_fds=True)
        except (OSError, ValueError):
            os.close(read_fd)
            os.close(write_fd)
            raise SlotProxyError("unavailable") from None
        finally:
            with contextlib.suppress(OSError):
                os.close(write_fd)
        return _Child(process, read_fd)

    def wait_ready(self, child, timeout: float) -> None:  # pragma: no cover
        """Block until the child says it is bound, it dies, or time runs out."""
        import select

        deadline = time.monotonic() + timeout
        while True:
            if child.process.poll() is not None:
                raise SlotProxyError("unavailable")
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise SlotProxyError("timeout")
            ready, _, _ = select.select([child.ready_read], [], [], min(remaining, 0.25))
            if not ready:
                continue
            try:
                token = os.read(child.ready_read, 1)
            except OSError:
                raise SlotProxyError("unavailable") from None
            if token == b"R":
                return
            # EOF without the token: the child exited before binding.
            raise SlotProxyError("unavailable")

    def stop(self, child, timeout: float) -> bool:  # pragma: no cover
        """Terminate, then kill, then reap. True only if the child is gone."""
        with contextlib.suppress(OSError):
            os.close(child.ready_read)
        process = child.process
        if process.poll() is not None:
            return True
        for send, budget in ((process.terminate, timeout), (process.kill, timeout)):
            try:
                send()
            except OSError as error:
                if error.errno == errno.ESRCH:
                    break
                # EPERM here is the CAP_KILL case: we cannot signal our own
                # uid-dropped child, so we cannot claim it is gone.
                return False
            try:
                process.wait(timeout=budget)
                return True
            except Exception:
                continue
        return process.poll() is not None


# --------------------------------------------------------------------------- #
# The lifecycle
# --------------------------------------------------------------------------- #


@contextlib.contextmanager
def slot_proxy(
    provider: str,
    purpose: str,
    slot: str,
    *,
    extra_hosts=(),
    launcher: Optional[ProxyLauncher] = None,
    lock=None,
    quarantine_dir: Optional[str] = None,
    ready_timeout: float = READY_TIMEOUT_SECONDS,
    stop_timeout: float = STOP_TIMEOUT_SECONDS,
):
    """Own this slot's proxy for the body, and leave nothing behind.

    Yields the `HTTP(S)_PROXY`/`NO_PROXY` environment the guest must run with.
    """
    extra_hosts = tuple(extra_hosts)
    validate_scope(provider, purpose, slot, extra_hosts=extra_hosts)
    directory = quarantine_dir or _QUARANTINE_DIR
    if is_quarantined(slot, directory=directory):
        # A previous tenant's proxy could not be confirmed dead.
        raise SlotProxyError("quarantined")
    launcher = launcher or ProxyLauncher()
    holder = lock if lock is not None else bootstrap.slot_proxy_lock
    try:
        held = holder(slot)
    except bootstrap.BootstrapError as error:
        # `busy` is the other owner holding the endpoint; anything else is ours.
        raise SlotProxyError(
            "busy" if getattr(error, "category", None) == "busy" else "invalid"
        ) from None
    with held:
        if launcher.listener_present(slot):
            # Under our exclusive lock this can only be a stray listener, and a
            # stray listener is serving an allowlist we did not build.
            raise SlotProxyError("occupied")
        # Only a run with integrations passes hosts, so a launcher (or test
        # double) written before them is called exactly as it always was.
        if extra_hosts:
            child = launcher.start(provider, purpose, slot, extra_hosts=extra_hosts)
        else:
            child = launcher.start(provider, purpose, slot)
        try:
            launcher.wait_ready(child, ready_timeout)
        except SlotProxyError:
            if not launcher.stop(child, stop_timeout):
                quarantine(slot, directory=directory)
            raise
        try:
            yield profile.proxy_env(slot)
        finally:
            # Always before the lock is dropped: the next tenant must never
            # find the previous proxy still bound.
            if not launcher.stop(child, stop_timeout):
                quarantine(slot, directory=directory)
                raise SlotProxyError("quarantined")


# --------------------------------------------------------------------------- #
# Child entry points (fixed internal CLI; not a general-purpose tool)
# --------------------------------------------------------------------------- #


# Linux `prctl` options and the capability-set layout the child gives up.
PR_CAPBSET_READ = 23
PR_CAPBSET_DROP = 24
PR_SET_NO_NEW_PRIVS = 38
PR_GET_NO_NEW_PRIVS = 39
PR_CAP_AMBIENT = 47
PR_CAP_AMBIENT_CLEAR_ALL = 4
CAP_SETPCAP = 8
_CAP_LAST_CAP_FALLBACK = 40
_LINUX_CAPABILITY_VERSION_3 = 0x20080522
_CAPABILITY_FIELDS = ("CapBnd", "CapEff", "CapPrm", "CapInh", "CapAmb")


class _SystemOps:  # pragma: no cover - Linux credentials
    """The real syscalls. A recording double replaces this in the tests."""

    def __init__(self):
        import ctypes

        self._ctypes = ctypes
        self._libc = ctypes.CDLL(None, use_errno=True)
        self._libc.prctl.restype = ctypes.c_int
        self._libc.prctl.argtypes = [ctypes.c_int] + [ctypes.c_ulong] * 4

    def geteuid(self):
        return os.geteuid()

    def getegid(self):
        return os.getegid()

    def prctl(self, option, arg2=0, arg3=0, arg4=0, arg5=0):
        return self._libc.prctl(option, arg2, arg3, arg4, arg5)

    def cap_last_cap(self):
        try:
            with open("/proc/sys/kernel/cap_last_cap", "rb") as handle:
                return int(handle.read().strip())
        except (OSError, ValueError):
            # Only affects which capabilities we walk; the final /proc check is
            # what actually decides whether the sets are empty.
            return _CAP_LAST_CAP_FALLBACK

    def setgroups(self, groups):
        os.setgroups(groups)

    def setresgid(self, gid):
        os.setresgid(gid, gid, gid)

    def setresuid(self, uid):
        os.setresuid(uid, uid, uid)

    def zero_capabilities(self):
        ctypes = self._ctypes

        class _Header(ctypes.Structure):
            _fields_ = [("version", ctypes.c_uint32), ("pid", ctypes.c_int)]

        class _Data(ctypes.Structure):
            _fields_ = [
                ("effective", ctypes.c_uint32),
                ("permitted", ctypes.c_uint32),
                ("inheritable", ctypes.c_uint32),
            ]

        header = _Header(_LINUX_CAPABILITY_VERSION_3, 0)
        data = (_Data * 2)()
        return self._libc.capset(ctypes.byref(header), ctypes.byref(data))

    def capability_state(self):
        return read_capability_state()


def read_capability_state(status_path: str = "/proc/self/status") -> dict:
    """The kernel's own view of this process: the five cap masks and NoNewPrivs.

    Read back rather than inferred. Every call that should have emptied a set
    can fail with its return value discarded, so the only trustworthy statement
    about the capabilities we hold is the one /proc makes.
    """
    wanted = set(_CAPABILITY_FIELDS) | {"NoNewPrivs"}
    state = {}
    try:
        with open(status_path, "r", encoding="utf-8", errors="replace") as handle:
            for line in handle:
                name, separator, value = line.partition(":")
                if not separator or name not in wanted:
                    continue
                value = value.strip()
                try:
                    state[name] = (
                        int(value) if name == "NoNewPrivs" else int(value, 16)
                    )
                except ValueError:
                    _refuse()
    except OSError:
        # Unverifiable is not the same as empty.
        _refuse()
    return state


def _refuse():
    raise SlotProxyError("unavailable")


def verify_dropped(state: dict) -> None:
    """Refuse unless every capability set is empty and no-new-privs is on.

    CapAmb may be absent on a kernel with no ambient capabilities at all, which
    is genuinely zero; the other four must be present, because a missing field
    means we did not read what we think we read.
    """
    for field in _CAPABILITY_FIELDS:
        if field not in state:
            if field == "CapAmb":
                continue
            _refuse()
        elif state[field] != 0:
            _refuse()
    if state.get("NoNewPrivs") != 1:
        _refuse()


def drop_privileges(ops=None) -> None:
    """Become uid/gid 62000 with no groups, no capabilities and no-new-privs.

    The order is the whole substance of this function.

    No-new-privs first, so nothing after it can regain privilege through an
    exec. Then the BOUNDING SET, while we are still root: `PR_CAPBSET_DROP`
    requires CAP_SETPCAP in the effective set, and the uid drop is exactly what
    takes CAP_SETPCAP away — dropping the bounding set afterwards returns EPERM
    for every capability and leaves it fully populated. Then groups, gid, uid,
    in that order, because dropping uid first would make the group calls fail
    and strand a half-dropped identity. Then ambient, then the remaining
    permitted/effective/inheritable sets, which anyone may always empty.

    Every return value is checked, and the kernel is asked at the end whether it
    agrees: a capability we failed to drop is not something to discover later
    from the process this is supposed to be confining. 3128 is unprivileged, so
    the bind that follows needs nothing retained.
    """
    ops = ops or _SystemOps()
    if ops.prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0:
        _refuse()

    if ops.geteuid() == 0:
        _drop_bounding_set(ops)
        ops.setgroups([])
        ops.setresgid(PROXY_GID)
        ops.setresuid(PROXY_UID)

    if ops.geteuid() != PROXY_UID or ops.getegid() != PROXY_GID:
        _refuse()

    # Ambient caps do not survive a uid change, but say so explicitly and check.
    if ops.prctl(PR_CAP_AMBIENT, PR_CAP_AMBIENT_CLEAR_ALL, 0, 0, 0) != 0:
        _refuse()
    if ops.zero_capabilities() != 0:
        _refuse()

    verify_dropped(ops.capability_state())


def _drop_bounding_set(ops) -> None:
    """Empty the bounding set. Must run while CAP_SETPCAP is still effective.

    A populated bounding set would let a later exec of a file-capable binary
    regain precisely what the uid drop just gave up.
    """
    last = ops.cap_last_cap()
    # CAP_SETPCAP last: it is the one authorising these drops. Removing it from
    # the BOUNDING set does not remove it from the effective set, so the order
    # is belt-and-braces rather than required — and cheap.
    order = [cap for cap in range(last + 1) if cap != CAP_SETPCAP] + [CAP_SETPCAP]
    for capability in order:
        present = ops.prctl(PR_CAPBSET_READ, capability, 0, 0, 0)
        if present < 0:
            # Not a capability this kernel knows; nothing to drop.
            continue
        if present == 0:
            continue
        if ops.prctl(PR_CAPBSET_DROP, capability, 0, 0, 0) != 0:
            # This is the EPERM that an ignored return value used to hide.
            _refuse()


def _serve(provider: str, purpose: str, host: str, port: int, ready_fd: int,
           extra_hosts=()) -> int:
    """Child: drop privileges, bind, signal ready, serve until terminated."""
    import asyncio

    if extra_hosts:
        policy = egress.policy_for(provider, purpose, extra_hosts=tuple(extra_hosts))
    else:
        policy = egress.policy_for(provider, purpose)
    drop_privileges()

    async def run():
        proxy = egress.ProviderEgressProxy(policy)
        server = await asyncio.start_server(
            proxy.handle, host, port, limit=egress.MAX_HEADER_BYTES + 1
        )
        # Only now is the endpoint real; telling the parent any earlier would
        # let a guest be handed a proxy that is not listening yet.
        if ready_fd >= 0:
            with contextlib.suppress(OSError):
                os.write(ready_fd, b"R")
                os.close(ready_fd)
        try:
            async with server:
                await server.serve_forever()
        finally:
            server.close()
            with contextlib.suppress(Exception):
                await server.wait_closed()

    try:
        asyncio.run(run())
    except (KeyboardInterrupt, SystemExit):
        # SIGTERM arriving mid-serve is the normal way this ends.
        pass
    return 0


def _probe(host: str, port: int) -> int:
    """0 listening, 1 definitely free, 3 undetermined.

    Only a refused connection proves nothing is bound. A timeout, an unreachable
    address or a resolver failure says we could not find out — reporting that as
    free is how a live proxy gets a second one started on top of it.
    """
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as probe:
        probe.settimeout(PROBE_TIMEOUT_SECONDS)
        try:
            probe.connect((host, port))
        except ConnectionRefusedError:
            return PROBE_FREE
        except OSError:
            return PROBE_UNDETERMINED
    return PROBE_LISTENING


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(
        prog="neev_slot_proxy",
        description="Internal entry point for the per-slot provider egress proxy.",
    )
    parser.add_argument("--serve", action="store_true")
    parser.add_argument("--probe", action="store_true")
    parser.add_argument("--provider")
    parser.add_argument("--purpose")
    parser.add_argument("--host")
    parser.add_argument("--port", type=int, default=PROXY_PORT)
    parser.add_argument("--ready-fd", type=int, default=-1)
    parser.add_argument("--extra-host", action="append", default=[], dest="extra_hosts")
    args = parser.parse_args(argv)
    if args.probe:
        if not args.host:
            return 2
        return _probe(args.host, args.port)
    if not args.serve:
        return 2
    if not _valid_scope_names(args.provider, args.purpose):
        return 2
    if not args.host:
        return 2
    extra_hosts = tuple(args.extra_hosts)
    try:
        egress.policy_for(args.provider, args.purpose, extra_hosts=extra_hosts)
    except egress.EgressError:
        return 2
    signal.signal(signal.SIGTERM, lambda *_: sys.exit(0))
    if extra_hosts:
        return _serve(args.provider, args.purpose, args.host, args.port, args.ready_fd,
                      extra_hosts=extra_hosts)
    return _serve(args.provider, args.purpose, args.host, args.port, args.ready_fd)


if __name__ == "__main__":  # pragma: no cover - process entry
    raise SystemExit(main())
