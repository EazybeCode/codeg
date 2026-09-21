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
import sys
import time
from dataclasses import dataclass
from typing import Optional

import neev_agent_network_bootstrap as bootstrap
import neev_login_network_profile as profile
import neev_provider_egress as egress


PROXY_PORT = 3128
# The proxy runs as nobody in particular: its own uid, no groups, no caps.
PROXY_UID = 62000
PROXY_GID = 62000

VALID_PROVIDERS = frozenset(("claude", "codex"))
VALID_PURPOSES = frozenset(("login", "run"))

# Bounded waits. Ready is short because the child only has to bind a socket;
# stop gives a SIGTERM time to unwind before the SIGKILL that root proved needs
# CAP_KILL to reach a uid-dropped child.
READY_TIMEOUT_SECONDS = 10.0
STOP_TIMEOUT_SECONDS = 5.0
_POLL_SECONDS = 0.02

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


def validate_scope(provider: str, purpose: str, slot: str) -> None:
    """Reject anything outside the supported matrix BEFORE spawning.

    `policy_for` is the authority on provider+purpose — it is what the child
    will build its allowlist from — so it is consulted here rather than
    duplicated, and a scope it denies never reaches a subprocess.
    """
    if type(provider) is not str or provider not in VALID_PROVIDERS:
        raise SlotProxyError("invalid")
    if type(purpose) is not str or purpose not in VALID_PURPOSES:
        raise SlotProxyError("invalid")
    if type(slot) is not str or slot not in ("a", "b"):
        raise SlotProxyError("invalid")
    try:
        egress.policy_for(provider, purpose)
    except Exception:
        raise SlotProxyError("invalid") from None


def proxy_bind_ip(slot: str) -> str:
    """The address this slot's proxy must listen on, from the fixed profile."""
    try:
        return profile.slot_profile(slot).proxy_ip
    except Exception:
        raise SlotProxyError("invalid") from None


def child_argv(provider: str, purpose: str, slot: str, ready_fd: int) -> list:
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
    ]


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

    def listener_present(self, slot: str) -> bool:  # pragma: no cover - Linux netns
        """True if something is already bound to this slot's proxy endpoint.

        Probed from inside the namespace, because that is the only place the
        address exists.
        """
        import subprocess

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
        return completed.returncode == 0

    def start(self, provider: str, purpose: str, slot: str):  # pragma: no cover
        import subprocess

        read_fd, write_fd = os.pipe()
        os.set_inheritable(write_fd, True)
        argv = child_argv(provider, purpose, slot, write_fd)
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
    launcher: Optional[ProxyLauncher] = None,
    lock=None,
    quarantine_dir: Optional[str] = None,
    ready_timeout: float = READY_TIMEOUT_SECONDS,
    stop_timeout: float = STOP_TIMEOUT_SECONDS,
):
    """Own this slot's proxy for the body, and leave nothing behind.

    Yields the `HTTP(S)_PROXY`/`NO_PROXY` environment the guest must run with.
    """
    validate_scope(provider, purpose, slot)
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


def drop_privileges() -> None:  # pragma: no cover - Linux credentials
    """Become uid/gid 62000 with no groups, no caps and no-new-privs.

    Order is load-bearing: no-new-privs first so nothing later can regain
    privilege through exec, then groups, then gid, then uid — dropping uid first
    would make the group calls fail and leave the process partly privileged.
    3128 is unprivileged, so the bind after this needs nothing retained.
    """
    import ctypes

    libc = ctypes.CDLL(None, use_errno=True)
    PR_SET_NO_NEW_PRIVS = 38
    if libc.prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0:
        raise SlotProxyError("unavailable")
    if os.geteuid() == 0:
        os.setgroups([])
        os.setresgid(PROXY_GID, PROXY_GID, PROXY_GID)
        os.setresuid(PROXY_UID, PROXY_UID, PROXY_UID)
    if os.geteuid() != PROXY_UID or os.getegid() != PROXY_GID:
        raise SlotProxyError("unavailable")
    _drop_all_capabilities(libc)


def _drop_all_capabilities(libc) -> None:  # pragma: no cover - Linux capabilities
    """Empty the permitted/effective/inheritable sets and the bounding set.

    After a uid change the effective set is already cleared, but the bounding
    set survives; leaving it populated would let a future exec of a file-capable
    binary regain what this just gave up.
    """
    import ctypes

    PR_CAPBSET_DROP = 24
    CAP_LAST_CAP = 40
    for capability in range(CAP_LAST_CAP + 1):
        libc.prctl(PR_CAPBSET_DROP, capability, 0, 0, 0)

    class _Header(ctypes.Structure):
        _fields_ = [("version", ctypes.c_uint32), ("pid", ctypes.c_int)]

    class _Data(ctypes.Structure):
        _fields_ = [
            ("effective", ctypes.c_uint32),
            ("permitted", ctypes.c_uint32),
            ("inheritable", ctypes.c_uint32),
        ]

    header = _Header(0x20080522, 0)  # _LINUX_CAPABILITY_VERSION_3
    data = (_Data * 2)()
    if libc.capset(ctypes.byref(header), ctypes.byref(data)) != 0:
        # An empty set that will not apply is a privilege we cannot account for.
        raise SlotProxyError("unavailable")


def _serve(provider: str, purpose: str, host: str, port: int, ready_fd: int) -> int:
    """Child: drop privileges, bind, signal ready, serve until terminated."""
    import asyncio

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
    """Exit 0 if something is already listening; 1 if the endpoint is free."""
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as probe:
        probe.settimeout(1.0)
        try:
            probe.connect((host, port))
        except (ConnectionRefusedError, socket.timeout, OSError):
            return 1
    return 0


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(add_help=False)
    parser.add_argument("--serve", action="store_true")
    parser.add_argument("--probe", action="store_true")
    parser.add_argument("--provider")
    parser.add_argument("--purpose")
    parser.add_argument("--host")
    parser.add_argument("--port", type=int, default=PROXY_PORT)
    parser.add_argument("--ready-fd", type=int, default=-1)
    args = parser.parse_args(argv)
    if args.probe:
        if not args.host:
            return 2
        return _probe(args.host, args.port)
    if not args.serve:
        return 2
    if args.provider not in VALID_PROVIDERS or args.purpose not in VALID_PURPOSES:
        return 2
    if not args.host:
        return 2
    signal.signal(signal.SIGTERM, lambda *_: sys.exit(0))
    return _serve(args.provider, args.purpose, args.host, args.port, args.ready_fd)


if __name__ == "__main__":  # pragma: no cover - process entry
    raise SystemExit(main())
